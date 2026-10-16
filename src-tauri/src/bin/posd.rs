//! `posd` — headless REST-only daemon.
//!
//! Runs the exact same settings + providers + HTTP API as the desktop app,
//! without any GUI. Useful from a terminal session or a systemd user service:
//!
//! ```ini
//! # ~/.config/systemd/user/posd.service
//! [Unit]
//! Description=Ledger POS REST bridge
//! [Service]
//! ExecStart=/usr/local/bin/posd
//! Restart=on-failure
//! [Install]
//! WantedBy=default.target
//! ```
//!
//! Note: the desktop app binds the same port, so run one or the other.
//!
//! It also carries the administrator commands for the application lock, which
//! write a root-owned file the app can only read:
//!
//! ```sh
//! sudo posd admin set-password
//! sudo posd admin clear-password
//! sudo posd admin auto-lock 15
//! sudo posd admin status
//! sudo posd admin events add https://example.com/hooks/pos [--events security.*,settings.changed]
//! sudo posd admin events list | remove <id> | enable <id> | disable <id> | test [<id>]
//! sudo posd admin receipt-key set [--kid <id>] | clear
//! ```
//!
//! The console runs the same commands through pkexec with `admin --json …` (and
//! `set-password --stdin`), so it reads a JSON object instead of the sentences.

use payment_pos_app_lib::security::{
    self, add_subscription_at, admin_path, clear_password_at, clear_receipt_key_at,
    parse_admin_command, remove_subscription_at, set_auto_lock_at, set_password_at,
    set_receipt_key_at, set_subscription_enabled_at, split_json_flag, status_report,
    try_load_from, AdminCommand,
};

/// Tell the old and the new endpoints about a subscription change, at once and
/// signed — see `events::announce_config_change`. `main` is already on a
/// (multi-threaded) runtime, so block on it in place rather than start a second.
fn announce(before: &security::AdminConfig, after: &security::AdminConfig) -> Vec<String> {
    let handle = tokio::runtime::Handle::current();
    tokio::task::block_in_place(|| {
        handle.block_on(payment_pos_app_lib::events::announce_config_change(
            before, after,
        ))
    })
}

fn announced_text(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!(
            "\nAnnounced (events.config_changed):\n  {}",
            lines.join("\n  ")
        )
    }
}

/// The administrator file lives outside any user's home on purpose; writing it
/// needs root, which is the whole point of keeping the hash there.
#[cfg(unix)]
fn require_root() -> Result<(), String> {
    // SAFETY: `geteuid` is always safe — it reads a process property and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        return Ok(());
    }
    Err(format!(
        "this command writes {} and must be run as root:\n\n    sudo posd admin …",
        admin_path().display()
    ))
}

#[cfg(not(unix))]
fn require_root() -> Result<(), String> {
    // Windows/macOS installs rely on the file's ACL; we cannot check it portably.
    Ok(())
}

fn check_length(password: &str) -> Result<(), String> {
    if password.chars().count() < security::MIN_PASSWORD_LEN {
        return Err(format!(
            "password must be at least {} characters; nothing was changed",
            security::MIN_PASSWORD_LEN
        ));
    }
    Ok(())
}

/// Ask twice, echo neither.
fn prompt_new_password() -> Result<String, String> {
    let first = rpassword::prompt_password("New application password: ")
        .map_err(|e| format!("cannot read from the terminal: {e}"))?;
    let again = rpassword::prompt_password("Repeat it: ")
        .map_err(|e| format!("cannot read from the terminal: {e}"))?;
    if first != again {
        return Err("the two entries did not match; nothing was changed".into());
    }
    check_length(&first)?;
    Ok(first)
}

/// One line from stdin — the console asked twice already and pipes the result in.
fn read_password_stdin() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("cannot read the password from stdin: {e}"))?;
    let password = line.trim_end_matches(['\n', '\r']).to_string();
    check_length(&password)?;
    Ok(password)
}

/// What a command printed: the sentence for a person, the object for `--json`.
struct Said {
    text: String,
    json: serde_json::Value,
}

fn said(text: impl Into<String>, json: serde_json::Value) -> Said {
    Said { text: text.into(), json }
}

fn run_admin(command: AdminCommand) -> Result<Said, String> {
    use serde_json::json;
    let path = admin_path();
    // `status` and `events list` only read, so they do not need root. `events test`
    // reads the secrets, which is root's business like writing them.
    if !matches!(command, AdminCommand::Status | AdminCommand::EventsList) {
        require_root()?;
    }
    match command {
        AdminCommand::Status => {
            let report = status_report(&path);
            Ok(said(report.clone(), json!({ "report": report })))
        }
        AdminCommand::SetPassword { stdin } => {
            let password = if stdin { read_password_stdin()? } else { prompt_new_password()? };
            set_password_at(&path, &password)?;
            Ok(said(
                format!(
                    "Password set in {}. Every open session has been invalidated.",
                    path.display()
                ),
                json!({ "ok": true }),
            ))
        }
        AdminCommand::ClearPassword => {
            clear_password_at(&path)?;
            Ok(said(
                format!(
                    "Password removed from {}. The application is open again.",
                    path.display()
                ),
                json!({ "ok": true }),
            ))
        }
        AdminCommand::AutoLock(minutes) => {
            set_auto_lock_at(&path, minutes)?;
            Ok(said(
                match minutes {
                    0 => "Auto-lock disabled.".to_string(),
                    m => format!("Auto-lock set to {m} idle minutes."),
                },
                json!({ "ok": true, "autoLockMinutes": minutes }),
            ))
        }
        AdminCommand::EventsAdd { url, events } => {
            let before = try_load_from(&path)?;
            let sub = add_subscription_at(&path, &url, events)?;
            let told = announce(&before, &try_load_from(&path)?);
            let plain = if url.starts_with("http://") {
                "\nNote: plain http — the payload travels unencrypted, though it stays signed.\n"
            } else {
                ""
            };
            Ok(said(
                format!(
                    "Subscription {} added for {} ({}).\n\
                     Signing secret — give it to the receiver now; it is not shown again:\n\n    {}\n\
                     {plain}\nThe running app picks this up within a few seconds. Try it:\n\
                     sudo posd admin events test {}{}",
                    sub.id,
                    sub.url,
                    sub.events.join(", "),
                    sub.secret,
                    sub.id,
                    announced_text(&told)
                ),
                json!({
                    "id": sub.id,
                    "url": sub.url,
                    "events": sub.events,
                    "secret": sub.secret,
                    "announced": told,
                }),
            ))
        }
        AdminCommand::EventsList => {
            let cfg = try_load_from(&path)?;
            let views: Vec<_> = cfg
                .events
                .subscriptions
                .iter()
                .map(|s| json!({ "id": s.id, "url": s.url, "events": s.events, "enabled": s.enabled }))
                .collect();
            let text = if cfg.events.subscriptions.is_empty() {
                "No event subscriptions.".to_string()
            } else {
                cfg.events
                    .subscriptions
                    .iter()
                    .map(|s| {
                        format!(
                            "{}  {}  [{}]{}",
                            s.id,
                            s.url,
                            s.events.join(","),
                            if s.enabled { "" } else { "  (disabled)" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(said(text, json!({ "subscriptions": views })))
        }
        AdminCommand::EventsRemove(id) => {
            let before = try_load_from(&path)?;
            remove_subscription_at(&path, &id)?;
            let told = announce(&before, &try_load_from(&path)?);
            Ok(said(
                format!(
                    "Subscription {id} removed; nothing more is sent there.{}",
                    announced_text(&told)
                ),
                json!({ "ok": true, "announced": told }),
            ))
        }
        AdminCommand::EventsEnable { id, enabled } => {
            let before = try_load_from(&path)?;
            set_subscription_enabled_at(&path, &id, enabled)?;
            let told = announce(&before, &try_load_from(&path)?);
            Ok(said(
                format!(
                    "Subscription {id} {}.{}",
                    if enabled { "enabled — deliveries resume" } else { "disabled — nothing is sent there until it is enabled again" },
                    announced_text(&told)
                ),
                json!({ "ok": true, "enabled": enabled, "announced": told }),
            ))
        }
        AdminCommand::EventsTest(id) => {
            let cfg = try_load_from(&path)?;
            let targets: Vec<_> = cfg
                .events
                .subscriptions
                .iter()
                .filter(|s| id.as_deref().is_none_or(|want| s.id == want))
                .cloned()
                .collect();
            if targets.is_empty() {
                return Err(match id {
                    Some(id) => format!("no subscription '{id}' (see `posd admin events list`)"),
                    None => "no event subscriptions to test".into(),
                });
            }
            // `main` is already on a (multi-threaded) runtime; a second one would panic.
            let handle = tokio::runtime::Handle::current();
            let mut lines = Vec::new();
            let mut results = Vec::new();
            for sub in targets {
                let outcome = tokio::task::block_in_place(|| {
                    handle.block_on(payment_pos_app_lib::events::send_test(
                        &sub,
                        cfg.events.device_id.as_deref(),
                    ))
                });
                let (line, result) = match outcome {
                    Ok(status) if (200..300).contains(&status) => (
                        format!("{}  {}  delivered ({status})", sub.id, sub.url),
                        json!({ "id": sub.id, "url": sub.url, "status": status, "delivered": true }),
                    ),
                    Ok(status) => (
                        format!("{}  {}  answered {status}", sub.id, sub.url),
                        json!({ "id": sub.id, "url": sub.url, "status": status, "delivered": false }),
                    ),
                    Err(e) => (
                        format!("{}  {}  not delivered: {e}", sub.id, sub.url),
                        json!({ "id": sub.id, "url": sub.url, "delivered": false, "error": e.to_string() }),
                    ),
                };
                lines.push(line);
                results.push(result);
            }
            Ok(said(lines.join("\n"), json!({ "results": results })))
        }
        AdminCommand::ReceiptKeySet(kid) => {
            let key = set_receipt_key_at(&path, kid)?;
            Ok(said(
                format!(
                    "Receipts are now signed (HS256) with key id {}.\n\
                     Key — give it to whoever verifies the receipts; it is not shown again:\n\n    {}\n\n\
                     Every successful invoke now carries an `attestation` token. Running this again\n\
                     rotates the key; `receipt-key clear` stops signing.",
                    key.kid, key.secret
                ),
                json!({ "kid": key.kid, "secret": key.secret }),
            ))
        }
        AdminCommand::ReceiptKeyClear => {
            clear_receipt_key_at(&path)?;
            Ok(said(
                "Receipt signing stopped; invoke replies carry no attestation.",
                json!({ "ok": true }),
            ))
        }
    }
}

#[tokio::main]
async fn main() {
    // Before anything is loaded: migrations it performs are recorded as posd's, and the
    // app's run state (`state.json` → `app`) is never touched from here.
    payment_pos_app_lib::version::set_process("posd");
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("posd {}", payment_pos_app_lib::version::VERSION);
        return;
    }
    // `--json` (right after `admin`): the console's machine-readable form. Errors stay
    // plain text on stderr either way.
    let (args, json) = split_json_flag(&args);
    if let Some(parsed) = parse_admin_command(&args) {
        match parsed.and_then(run_admin) {
            Ok(out) if json => println!("{}", out.json),
            Ok(out) => println!("{}", out.text),
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(2);
            }
        }
        return;
    }

    payment_pos_app_lib::init_tracing();
    let state = payment_pos_app_lib::build_state();
    let settings = state.settings.get();
    eprintln!(
        "posd {} — REST bridge on http://{}:{} (docs at /docs), settings: {}",
        env!("CARGO_PKG_VERSION"),
        settings.server.host,
        settings.server.port,
        payment_pos_app_lib::settings::settings_path().display(),
    );

    // Webhook events: idle unless an administrator configured a subscription.
    tokio::spawn(payment_pos_app_lib::events::run(state.clone()));

    tokio::select! {
        _ = payment_pos_app_lib::server::run_server(state.clone()) => {}
        _ = stop_signal() => {
            eprintln!("\nposd: shutting down");
            payment_pos_app_lib::events::shutdown(&state, std::time::Duration::from_secs(3)).await;
        }
    }
}

/// Ctrl-C, or — the way systemd stops a service — SIGTERM. Either one sends
/// `app.stopping`; a SIGKILL cannot, and a receiver must not wait for it.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

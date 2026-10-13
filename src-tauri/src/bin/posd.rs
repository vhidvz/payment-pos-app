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
//! sudo posd admin events list | remove <id> | test [<id>]
//! sudo posd admin receipt-key set [--kid <id>] | clear
//! ```

use payment_pos_app_lib::security::{
    self, add_subscription_at, admin_path, clear_password_at, clear_receipt_key_at,
    parse_admin_command, remove_subscription_at, set_auto_lock_at, set_password_at,
    set_receipt_key_at, status_report, try_load_from, AdminCommand,
};

/// Tell the old and the new endpoints about a subscription change, at once and
/// signed — see `events::announce_config_change`. `main` is already on a
/// (multi-threaded) runtime, so block on it in place rather than start a second.
fn announce(before: &security::AdminConfig, after: &security::AdminConfig) -> String {
    let handle = tokio::runtime::Handle::current();
    let lines = tokio::task::block_in_place(|| {
        handle.block_on(payment_pos_app_lib::events::announce_config_change(
            before, after,
        ))
    });
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

/// Ask twice, echo neither.
fn prompt_new_password() -> Result<String, String> {
    let first = rpassword::prompt_password("New application password: ")
        .map_err(|e| format!("cannot read from the terminal: {e}"))?;
    let again = rpassword::prompt_password("Repeat it: ")
        .map_err(|e| format!("cannot read from the terminal: {e}"))?;
    if first != again {
        return Err("the two entries did not match; nothing was changed".into());
    }
    if first.chars().count() < security::MIN_PASSWORD_LEN {
        return Err(format!(
            "password must be at least {} characters; nothing was changed",
            security::MIN_PASSWORD_LEN
        ));
    }
    Ok(first)
}

fn run_admin(command: AdminCommand) -> Result<String, String> {
    let path = admin_path();
    // `status` and `events list` only read, so they do not need root. `events test`
    // reads the secrets, which is root's business like writing them.
    if !matches!(command, AdminCommand::Status | AdminCommand::EventsList) {
        require_root()?;
    }
    match command {
        AdminCommand::Status => Ok(status_report(&path)),
        AdminCommand::SetPassword => {
            let password = prompt_new_password()?;
            set_password_at(&path, &password)?;
            Ok(format!(
                "Password set in {}. Every open session has been invalidated.",
                path.display()
            ))
        }
        AdminCommand::ClearPassword => {
            clear_password_at(&path)?;
            Ok(format!(
                "Password removed from {}. The application is open again.",
                path.display()
            ))
        }
        AdminCommand::AutoLock(minutes) => {
            set_auto_lock_at(&path, minutes)?;
            Ok(match minutes {
                0 => "Auto-lock disabled.".to_string(),
                m => format!("Auto-lock set to {m} idle minutes."),
            })
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
            Ok(format!(
                "Subscription {} added for {} ({}).\n\
                 Signing secret — give it to the receiver now; it is not shown again:\n\n    {}\n\
                 {plain}\nThe running app picks this up within a few seconds. Try it:\n\
                 sudo posd admin events test {}{told}",
                sub.id,
                sub.url,
                sub.events.join(", "),
                sub.secret,
                sub.id
            ))
        }
        AdminCommand::EventsList => {
            let cfg = try_load_from(&path)?;
            if cfg.events.subscriptions.is_empty() {
                return Ok("No event subscriptions.".into());
            }
            Ok(cfg
                .events
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
                .join("\n"))
        }
        AdminCommand::EventsRemove(id) => {
            let before = try_load_from(&path)?;
            remove_subscription_at(&path, &id)?;
            let told = announce(&before, &try_load_from(&path)?);
            Ok(format!(
                "Subscription {id} removed; nothing more is sent there.{told}"
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
            for sub in targets {
                let outcome = tokio::task::block_in_place(|| {
                    handle.block_on(payment_pos_app_lib::events::send_test(
                        &sub,
                        cfg.events.device_id.as_deref(),
                    ))
                });
                lines.push(match outcome {
                    Ok(status) if (200..300).contains(&status) => {
                        format!("{}  {}  delivered ({status})", sub.id, sub.url)
                    }
                    Ok(status) => format!("{}  {}  answered {status}", sub.id, sub.url),
                    Err(e) => format!("{}  {}  not delivered: {e}", sub.id, sub.url),
                });
            }
            Ok(lines.join("\n"))
        }
        AdminCommand::ReceiptKeySet(kid) => {
            let key = set_receipt_key_at(&path, kid)?;
            Ok(format!(
                "Receipts are now signed (HS256) with key id {}.\n\
                 Key — give it to whoever verifies the receipts; it is not shown again:\n\n    {}\n\n\
                 Every successful invoke now carries an `attestation` token. Running this again\n\
                 rotates the key; `receipt-key clear` stops signing.",
                key.kid, key.secret
            ))
        }
        AdminCommand::ReceiptKeyClear => {
            clear_receipt_key_at(&path)?;
            Ok("Receipt signing stopped; invoke replies carry no attestation.".into())
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
    if let Some(parsed) = parse_admin_command(&args) {
        match parsed.and_then(run_admin) {
            Ok(message) => println!("{message}"),
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

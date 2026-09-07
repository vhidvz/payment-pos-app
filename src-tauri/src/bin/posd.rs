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
//! ```

use payment_pos_app_lib::security::{
    self, admin_path, clear_password_at, parse_admin_command, set_auto_lock_at, set_password_at,
    status_report, AdminCommand,
};

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
    // `status` only reads, so it does not need root.
    if !matches!(command, AdminCommand::Status) {
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
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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

    tokio::select! {
        _ = payment_pos_app_lib::server::run_server(state) => {}
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nposd: shutting down");
        }
    }
}

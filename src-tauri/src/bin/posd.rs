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

#[tokio::main]
async fn main() {
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

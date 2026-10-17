//! The version this binary is, and which process is running it.

use std::sync::OnceLock;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

static PROCESS: OnceLock<&'static str> = OnceLock::new();

pub fn current() -> semver::Version {
    semver::Version::parse(VERSION).expect("Cargo.toml carries a semver version")
}

/// `posd` calls this first thing; the desktop app never does, so it stays `app`.
pub fn set_process(name: &'static str) {
    let _ = PROCESS.set(name);
}

pub fn is_app() -> bool {
    process_name() == "app"
}

/// `posd` or `app` — `/api/v1/health` reports it, so a prober knows who answers.
pub fn process_name() -> &'static str {
    PROCESS.get().copied().unwrap_or("app")
}

/// Who did something, for `state.json`'s `migrations[].by`: `app 1.0.0`, `posd 1.0.0`.
pub fn process_label() -> String {
    format!("{} {VERSION}", PROCESS.get().copied().unwrap_or("app"))
}

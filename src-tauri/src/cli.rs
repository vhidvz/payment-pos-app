//! Command-line verbs, handled before any window opens (spec §7).

use crate::install::{self, Layout, Source};
use crate::update::{self, engine, Boot};
use crate::version;

#[derive(Debug, PartialEq, Eq)]
pub enum Cli {
    Version,
    Install { allow_downgrade: bool },
    Update,
    Rollback,
    Uninstall,
    InstallStatus,
    Gui { background: bool },
}

pub fn parse(args: &[String]) -> Cli {
    let has = |f: &str| args.iter().any(|a| a == f);
    match args.first().map(String::as_str) {
        Some("--version") => Cli::Version,
        Some("--install") => Cli::Install { allow_downgrade: has("--allow-downgrade") },
        Some("--update") => Cli::Update,
        Some("--rollback") => Cli::Rollback,
        Some("--uninstall") => Cli::Uninstall,
        Some("--install-status") => Cli::InstallStatus,
        _ => Cli::Gui { background: has("--background") || has("--minimized") },
    }
}

/// Exit codes: 0 done · 1 failed · 2 not an AppImage · 3 refused (newer/unknown) · 4 blocked.
pub fn run(cli: Cli) -> i32 {
    match cli {
        Cli::Version => {
            println!("Ledger POS {}", version::VERSION);
            0
        }
        Cli::InstallStatus => status(),
        Cli::Uninstall => uninstall(),
        Cli::Install { allow_downgrade } => install(allow_downgrade),
        Cli::Update => update(),
        Cli::Rollback => rollback(),
        Cli::Gui { .. } => 0,
    }
}

fn appimage_or_exit() -> Result<Source, i32> {
    Source::detect().ok_or_else(|| {
        eprintln!("This only applies to the AppImage build; this copy was started another way.");
        2
    })
}

/// Shared policy: refuse a newer or unknown install unless explicitly allowed (spec §7).
fn refusal(layout: &Layout, allow_downgrade: bool) -> Option<String> {
    match update::plan(&version::current(), &install::installed(layout), true, false) {
        Boot::NewerInstalled { installed } if !allow_downgrade => Some(format!(
            "Ledger POS {installed} is installed — newer than this file ({}). Nothing was changed; \
             pass --allow-downgrade to replace it anyway.",
            version::VERSION
        )),
        Boot::ReplaceUnknown { reason } if !allow_downgrade => Some(format!(
            "The installed copy's version cannot be determined ({reason}). Nothing was changed; \
             pass --allow-downgrade to replace it anyway."
        )),
        _ => None,
    }
}

/// The background API after an install or a rollback: one line, and whether it is up.
fn report_service(status: Option<crate::daemon::Status>) {
    match status.map(|s| s.describe()) {
        None => println!("Background API: none — this version serves from its own window."),
        Some(Ok(line)) => println!("Background API (posd): {line}"),
        Some(Err(line)) => eprintln!("Background API (posd) did not start: {line}"),
    }
}

fn install(allow_downgrade: bool) -> i32 {
    let source = match appimage_or_exit() {
        Ok(s) => s,
        Err(code) => return code,
    };
    let layout = Layout::current();
    let _ = install::txn::recover(&layout);
    if let Some(why) = refusal(&layout, allow_downgrade) {
        eprintln!("{why}");
        return 3;
    }
    match install::install(&layout, &source) {
        Ok(report) => {
            let user = std::env::var("USER").unwrap_or_else(|_| "this user".into());
            println!("Installed Ledger POS {} for {user}:", version::VERSION);
            for p in report.written {
                println!("  {}", p.display());
            }
            // The API runs from login on, window or not — this is all a headless box needs.
            report_service(Some(crate::daemon::real::ensure_now()));
            println!("\nLauncher: your application menu. Administrator commands: posd admin status");
            println!("If `posd` is not found, add ~/.local/bin to your PATH.");
            println!("If the Ledger POS window is open, restart it to run {}.", version::VERSION);
            0
        }
        Err(e) => {
            eprintln!("install failed: {e}");
            1
        }
    }
}

fn update() -> i32 {
    let source = match appimage_or_exit() {
        Ok(s) => s,
        Err(code) => return code,
    };
    let layout = Layout::current();
    let _ = install::txn::recover(&layout);
    if let Some(why) = refusal(&layout, false) {
        eprintln!("{why}\nTo replace it, use --install --allow-downgrade.");
        return 3;
    }
    let installed = install::installed(&layout);
    let sys = crate::update::system::RealSystem::new();
    let outcome = engine::run_update(&sys, &layout, &source, &installed, &engine::Timeouts::default(), &mut |p| {
        println!("[{:?}] {:?}: {}", p.step, p.state, p.message)
    });
    match outcome {
        engine::EngineOutcome::Started { .. } => {
            println!("{}", outcome.describe());
            0
        }
        other => {
            eprintln!("{}", other.describe());
            4
        }
    }
}

fn rollback() -> i32 {
    let layout = Layout::current();
    match install::rollback(&layout) {
        Ok(version) => {
            println!("Rolled back to {version}. Restart the Ledger POS window to run it. Data files were not changed.");
            report_service(crate::daemon::real::after_rollback(&version));
            0
        }
        Err(e) => {
            eprintln!("rollback failed: {e}");
            1
        }
    }
}

fn uninstall() -> i32 {
    // Before the files: the service is named after the `posd` it runs.
    crate::daemon::real::remove_now();
    match install::uninstall(&Layout::current()) {
        Ok(report) if report.removed.is_empty() => {
            println!("Nothing was installed.");
            0
        }
        Ok(report) => {
            println!("Removed:");
            for p in report.removed {
                println!("  {}", p.display());
            }
            println!("\nYour settings and the administrator file were left untouched.");
            0
        }
        Err(e) => {
            eprintln!("uninstall failed: {e}");
            1
        }
    }
}

fn status() -> i32 {
    let layout = Layout::current();
    println!("this file  : {}", version::VERSION);
    println!("appimage   : {}", Source::detect().is_some());
    println!("installed  : {}", install::installed_label(&install::installed(&layout)));
    println!("previous   : {}", install::txn::prev_version(&layout).unwrap_or_else(|| "none".into()));
    println!(
        "journal    : {}",
        if layout.journal_path().exists() { "present — an install did not finish" } else { "none" }
    );
    println!("app        : {}", layout.app_path().display());
    println!("desktop    : {}", layout.desktop_path().display());
    println!("posd       : {}", layout.posd_path().display());
    let paths = crate::daemon::real::paths();
    let host = crate::daemon::real::RealHost::new();
    let api = match crate::daemon::Host::health(&host) {
        Some(h) => format!("{} {} answers", h.process.as_deref().unwrap_or("an older Ledger POS"), h.version),
        None => "nothing answers".into(),
    };
    let mode = crate::daemon::mode_in_use(&host, &paths).map_or("not set up", |m| m.label());
    println!("service    : {} ({mode}), {api}", paths.unit_name);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Cli {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn verbs() {
        assert_eq!(p(&["--version"]), Cli::Version);
        assert_eq!(p(&["--install"]), Cli::Install { allow_downgrade: false });
        assert_eq!(p(&["--install", "--allow-downgrade"]), Cli::Install { allow_downgrade: true });
        assert_eq!(p(&["--update"]), Cli::Update);
        assert_eq!(p(&["--rollback"]), Cli::Rollback);
        assert_eq!(p(&["--uninstall"]), Cli::Uninstall);
        assert_eq!(p(&["--install-status"]), Cli::InstallStatus);
        assert_eq!(p(&[]), Cli::Gui { background: false });
        assert_eq!(p(&["--background"]), Cli::Gui { background: true });
        assert_eq!(p(&["--minimized"]), Cli::Gui { background: true });
        // Only ever meaningful over D-Bus; typed by hand it just starts the app.
        assert_eq!(p(&["--quit-for-update"]), Cli::Gui { background: false });
    }
}

//! Ledger POS — desktop shell.
//!
//! The Tauri layer is deliberately thin, and it serves nothing: the REST API is the
//! headless `posd` binary's alone, run as a background service ([`daemon`]). The shell is
//! the console — window + tray, minimize-to-tray on close, start-at-boot syncing,
//! single-instance focus — and the manager that keeps `posd` installed and answering.

pub mod activity;
pub mod admission;
pub mod cli;
pub mod daemon;
pub mod elevate;
pub mod events;
pub mod fsutil;
pub mod install;
pub mod migrate;
pub mod posture;
pub mod providers;
pub mod receipts;
pub mod runstate;
pub mod security;
pub mod server;
pub mod settings;
pub mod update;
pub mod version;

use std::sync::Arc;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
#[cfg(not(target_os = "linux"))]
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::providers::{saman::SamanProvider, sandbox::SandboxProvider, ProviderRegistry};
use crate::server::AppState;
use crate::security::AuthState;
use crate::settings::SettingsStore;

pub fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();
}

/// Build the shared application state: settings store + provider registry.
pub fn build_state() -> AppState {
    let settings = Arc::new(SettingsStore::load());
    let s = settings.get();
    let cfg = |id: &str| {
        s.providers
            .configs
            .get(id)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    let registry = Arc::new(ProviderRegistry::new(vec![
        Arc::new(SamanProvider::new(cfg(providers::saman::PROVIDER_ID))),
        Arc::new(SandboxProvider::new(cfg(providers::sandbox::PROVIDER_ID))),
    ]));
    let auth = Arc::new(AuthState::new(crate::security::admin_path()));
    let mut state = AppState::new(settings.clone(), registry, auth);
    // Undelivered webhook messages survive a restart in the real app.
    state.events = Arc::new(events::EventBus::new(Some(events::EventBus::default_outbox_path())));
    // Run state after the file migrations, so this start's migrations are recorded and the
    // app's notice sees them (spec §4). `posd` records migrations but never touches `app`.
    let run_state = Arc::new(runstate::StateStore::open(runstate::state_path()));
    let mut migrated = settings.migrations();
    migrated.extend(state.events.migrations());
    run_state.record_migrations(&migrated);
    if version::is_app() {
        run_state.note_app_start(version::VERSION);
    }
    state.run_state = run_state;
    state.serving_file = Some(server::serving_path());
    state
}

// ------------------------------------------------------------- the console

/// What the console holds. It serves nothing: `posd` owns the API, the settings file and
/// the webhook outbox; the console reads settings ([`settings::peek`]) and manages `posd`.
#[derive(Clone)]
struct Shell {
    /// Drain gate for the console's own commands (install, rollback…) — an update waits on it.
    admission: Arc<admission::Admission>,
    /// `state.json`'s `app` section is the one file the console writes (locked, re-read).
    run_state: Arc<runstate::StateStore>,
    daemon: Arc<daemon::supervisor::Supervisor>,
    /// This start installed the AppImage by itself; the frontend says so once.
    just_installed: bool,
}

fn current_settings() -> settings::Settings {
    settings::peek(&settings::settings_path())
}

/// The first run of an AppImage with nothing installed installs it — the background API
/// needs a `posd` that outlives this file's mount. Nothing to ask: it is all in the user's
/// home, and Settings can remove it again.
fn auto_install() -> bool {
    let Some(source) = install::Source::detect() else { return false };
    let layout = install::Layout::current();
    if !matches!(install::installed(&layout), install::Installed::None) {
        return false;
    }
    match install::install(&layout, &source) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("could not install this AppImage ({e}); the API runs only while it is open");
            false
        }
    }
}

// --------------------------------------------------------- desktop install

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct InstallInfo {
    /// True only when this run can integrate itself — i.e. it is an AppImage.
    /// A packaged install has nothing to do here.
    available: bool,
    installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_path: Option<String>,
    /// This start installed it, unasked.
    just_installed: bool,
    /// The version kept for one rollback, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_version: Option<String>,
}

fn install_info(shell: &Shell) -> InstallInfo {
    let layout = install::Layout::current();
    InstallInfo {
        available: install::Source::detect().is_some(),
        installed: install::is_installed(&layout),
        installed_path: install::is_installed(&layout)
            .then(|| layout.app_path().display().to_string()),
        just_installed: shell.just_installed,
        previous_version: install::txn::previous_version(&layout),
    }
}

#[tauri::command]
fn install_status(shell: tauri::State<'_, Shell>) -> InstallInfo {
    install_info(&shell)
}

/// Settings' "Install" — after an uninstall, or when the automatic install failed.
#[tauri::command]
fn install_app(app: AppHandle, shell: tauri::State<'_, Shell>) -> Result<InstallInfo, String> {
    let _ticket = shell.admission.enter().map_err(|_| "updating".to_string())?;
    let source = install::Source::detect()
        .ok_or("this build is not an AppImage, so there is nothing to integrate")?;
    let layout = install::Layout::current();
    install::install(&layout, &source)?;
    // An autostart entry written before the install points at the downloaded file.
    if current_settings().app.start_at_boot {
        sync_autostart(&app, true);
    }
    // After an uninstall this session ran posd as its child; now it is a service again.
    shell.daemon.restore();
    Ok(install_info(&shell))
}

#[tauri::command]
async fn uninstall_app(app: AppHandle) -> Result<InstallInfo, String> {
    let shell = app.state::<Shell>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ticket = shell.admission.enter().map_err(|_| "updating".to_string())?;
        let posd = daemon::real::target_for(false).exe;
        install::uninstall(&install::Layout::current())?;
        // The service ran the copy just removed; until this window closes, ours serves.
        shell.daemon.go_ephemeral(&posd);
        Ok(install_info(&shell))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `--rollback` from the console: the previous version goes back on the menu and runs from
/// the next start; the background service moves onto its `posd` now (or, for a version from
/// before the service, is removed — that version serves from its own window).
#[tauri::command]
async fn rollback_install(app: AppHandle) -> Result<String, String> {
    let shell = app.state::<Shell>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ticket = shell.admission.enter().map_err(|_| "updating".to_string())?;
        let posd = daemon::real::target_for(false).exe;
        let version = install::rollback(&install::Layout::current())?;
        match semver::Version::parse(&version) {
            Ok(v) if (v.major, v.minor) < daemon::DAEMON_SINCE => shell.daemon.go_ephemeral(&posd),
            _ => shell.daemon.wake(),
        }
        Ok(version)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ------------------------------------------------------- administrator actions

/// Whether the console can ask the OS for the administrator here, and the terminal form.
#[tauri::command]
fn admin_preview(op: elevate::AdminOp) -> elevate::AdminPreview {
    elevate::preview(&op)
}

/// Run one administrator action through the OS's own password dialog ([`elevate`]).
#[tauri::command]
async fn admin_run(op: elevate::AdminOp) -> elevate::AdminRunResult {
    tauri::async_runtime::spawn_blocking(move || elevate::run(&op))
        .await
        .unwrap_or_else(|e| elevate::AdminRunResult::Failed {
            message: e.to_string(),
            command: String::new(),
        })
}

// ---------------------------------------------------- versions and updates

#[tauri::command]
fn boot_mode(info: tauri::State<'_, update::BootInfo>) -> update::BootInfo {
    info.inner().clone()
}

/// Read fresh: `posd` records the migrations it performs after the console started.
#[tauri::command]
fn version_notice() -> Option<runstate::NoticeView> {
    runstate::StateStore::open(runstate::state_path()).notice()
}

#[tauri::command]
fn ack_version_notice(shell: tauri::State<'_, Shell>) -> Result<(), String> {
    let _ticket = shell.admission.enter().map_err(|_| "updating".to_string())?;
    shell.run_state.ack()
}

/// The update screen's world: where to install, and the machine it acts on.
struct UpdateCtx {
    layout: install::Layout,
    sys: update::system::RealSystem,
}

fn emitter(app: &AppHandle) -> impl FnMut(update::engine::Progress) + '_ {
    use tauri::Emitter;
    move |p| {
        let _ = app.emit("update://progress", &p);
    }
}

#[tauri::command]
async fn start_update(app: AppHandle) -> Result<update::engine::EngineOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let ctx = app.state::<UpdateCtx>();
        let source = install::Source::detect().ok_or("this copy is not an AppImage")?;
        let installed = install::installed(&ctx.layout);
        let t = update::engine::Timeouts::default();
        Ok(update::engine::run_update(&ctx.sys, &ctx.layout, &source, &installed, &t, &mut emitter(&app)))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn relaunch_update(app: AppHandle) -> Result<update::engine::EngineOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let ctx = app.state::<UpdateCtx>();
        let t = update::engine::Timeouts::default();
        Ok(update::engine::relaunch(&ctx.sys, &ctx.layout, &t, &mut emitter(&app)))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// After a launch failure: put the previous version back, then start it.
#[tauri::command]
async fn rollback_update(app: AppHandle) -> Result<update::engine::EngineOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let ctx = app.state::<UpdateCtx>();
        let version = install::rollback(&ctx.layout)?;
        let _ = daemon::real::after_rollback(&version);
        let t = update::engine::Timeouts::default();
        Ok(update::engine::relaunch(&ctx.sys, &ctx.layout, &t, &mut emitter(&app)))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// "Not now" / "Open installed version": start (or surface) the installed copy, then leave.
#[tauri::command]
fn open_installed(app: AppHandle) -> Result<(), String> {
    use update::system::System;
    let ctx = app.state::<UpdateCtx>();
    ctx.sys.spawn_detached(&ctx.layout.app_path())?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

// ------------------------------------------------------------- UI commands

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct UiServerInfo {
    /// The background service answers on the configured address.
    running: bool,
    host: String,
    port: u16,
    base_url: String,
    docs_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    version: String,
    /// How `posd` runs and what the last check found.
    service: daemon::Status,
}

fn ui_server_info(shell: &Shell) -> UiServerInfo {
    // Where posd really serves — after a failed port change, the last address that worked —
    // so the console can always reach it to put the settings right.
    let (host, port) = daemon::real::api_endpoint();
    let service = shell.daemon.status();
    let connect_host = if host.contains(':') { format!("[{host}]") } else { host.clone() };
    let base_url = format!("http://{connect_host}:{port}");
    UiServerInfo {
        running: service.state == "serving",
        host,
        port,
        docs_url: format!("{base_url}/docs"),
        base_url,
        error: service.detail.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        service,
    }
}

#[tauri::command]
fn server_info(shell: tauri::State<'_, Shell>) -> UiServerInfo {
    ui_server_info(&shell)
}

#[tauri::command]
fn open_docs(app: AppHandle, shell: tauri::State<'_, Shell>) -> Result<(), String> {
    let info = ui_server_info(&shell);
    app.opener()
        .open_url(info.docs_url, None::<&str>)
        .map_err(|e| e.to_string())
}

// ------------------------------------------------------------------- shell

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Open Ledger POS", true, None::<&str>)?;
    let docs = MenuItem::with_id(app, "docs", "API documentation", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    // The API is `posd`'s: quitting the console leaves it serving.
    let quit = MenuItem::with_id(app, "quit", "Close console (the API keeps running)", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &docs, &sep, &quit])?;

    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().expect("window icon").clone())
        .tooltip("Ledger POS — payment terminal bridge")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "docs" => {
                let url = ui_server_info(&app.state::<Shell>()).docs_url;
                let _ = app.opener().open_url(url, None::<&str>);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Linux: write the XDG autostart entry ourselves instead of going through the
/// autostart plugin, whose Linux backend emits `Exec=<path> <args>` unquoted —
/// a path with a space (like the bundled `Ledger POS_*.AppImage`) yields an
/// entry desktop environments refuse to parse, so nothing launches at login.
/// Rewriting on every call also keeps the path fresh if the AppImage moves.
#[cfg(target_os = "linux")]
fn apply_autostart(app: &AppHandle, enabled: bool) -> std::io::Result<()> {
    let name = app.package_info().name.clone();
    let dir = dirs::config_dir()
        .ok_or_else(|| std::io::Error::other("no XDG config directory"))?
        .join("autostart");
    // Same file name the autostart plugin used, so entries it wrote are replaced.
    let file = dir.join(format!("{name}.desktop"));
    if !enabled {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let layout = install::Layout::current();
    let exe = match app.env().appimage {
        // The installed copy when there is one, not the file it was installed from.
        Some(_) if install::is_installed(&layout) => layout.app_path(),
        // The AppImage path, not the transient /tmp/.mount_* exe inside it.
        Some(appimage) => std::path::PathBuf::from(appimage),
        None => std::env::current_exe()?,
    };
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        &file,
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Version=1.0\n\
             Name={name}\n\
             Comment={name} — the console in the tray at login (the API runs regardless)\n\
             Exec={} --background\n\
             StartupNotify=false\n\
             Terminal=false\n",
            install::quote_exec(&exe)
        ),
    )
}

fn sync_autostart(app: &AppHandle, enabled: bool) {
    #[cfg(target_os = "linux")]
    if let Err(e) = apply_autostart(app, enabled) {
        tracing::warn!("failed to apply start-at-boot: {e}");
    }
    #[cfg(not(target_os = "linux"))]
    {
        let autostart = app.autolaunch();
        let result = if enabled { autostart.enable() } else { autostart.disable() };
        if let Err(e) = result {
            // Disabling when never enabled fails on some platforms — only surface real trouble.
            if enabled {
                tracing::warn!("failed to apply start-at-boot: {e}");
            }
        }
    }
}

/// Work around WebKitGTK rendering corruption on weak or quirky GPU stacks.
///
/// The DMA-BUF renderer draws streaked/banded frames on several Mesa and
/// proprietary drivers, and on ARM boards (Raspberry Pi class) accelerated
/// compositing itself is the usual source of artifacts and crashes. Both
/// variables are only defaulted — exporting them before launch still wins.
#[cfg(target_os = "linux")]
fn tune_webview_env() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
    if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }
}

#[cfg(not(target_os = "linux"))]
fn tune_webview_env() {}

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let background = match cli::parse(&args) {
        cli::Cli::Gui { background } => background,
        // `--install`, `--update`, `--version`… are handled before any window opens, so the
        // same binary can be driven from a provisioning script.
        verb => std::process::exit(cli::run(verb)),
    };
    init_tracing();
    tune_webview_env();
    let layout = install::Layout::current();
    if let Err(e) = install::txn::recover(&layout) {
        tracing::warn!("an interrupted install could not be finished ({e}); the update screen offers a repair");
    }
    // Decided before Tauri starts: once the single-instance plugin runs, a launch that finds
    // an older copy in the tray is handed to it and this process exits (spec §3.1).
    let source = install::Source::detect();
    let from_installed = source.as_ref().is_some_and(|s| update::same_file(&s.appimage, &layout.app_path()));
    match update::plan(&version::current(), &install::installed(&layout), source.is_some(), from_installed) {
        update::Boot::Normal => run_normal(background),
        boot => run_update_mode(boot, layout),
    }
}

/// An update screen instead of the app (spec §3.1): no single-instance, no tray — and no
/// settings loaded, so nothing migrates until the new version's `posd` runs.
fn run_update_mode(boot: update::Boot, layout: install::Layout) {
    use update::system::System;
    let sys = update::system::RealSystem::new();
    let info = update::BootInfo::from_boot(&boot, sys.instance_pid().is_some());
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(info)
        .manage(UpdateCtx { layout, sys })
        .invoke_handler(tauri::generate_handler![
            boot_mode,
            start_update,
            relaunch_update,
            rollback_update,
            open_installed,
            quit_app
        ])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title("Update Ledger POS");
                let _ = window.show();
                // End-to-end builds only: click an element by CSS selector through the
                // webview itself, so the harness drives the real button → command → events
                // path. Injected pointer input does not reach windows on every desktop.
                #[cfg(feature = "e2e")]
                if let Ok(selector) = std::env::var("LEDGER_POS_E2E_CLICK") {
                    let window = window.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(4));
                        let js = format!("document.querySelector({selector:?})?.click()");
                        let _ = window.eval(&js);
                    });
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the update screen");
}

fn run_normal(background: bool) {
    let just_installed = auto_install();
    let run_state = Arc::new(runstate::StateStore::open(runstate::state_path()));
    run_state.note_app_start(version::VERSION);
    let shell = Shell {
        admission: Arc::new(admission::Admission::new()),
        run_state,
        daemon: daemon::supervisor::Supervisor::new(),
        just_installed,
    };
    let daemon = shell.daemon.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == update::system::QUIT_FOR_UPDATE) {
                // An update is replacing this copy (spec §3.4): let the console's own commands
                // finish, then exit. The API is `posd`'s; the update restarts it.
                let shell = app.state::<Shell>().inner().clone();
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    if !shell.admission.drain(std::time::Duration::from_secs(300)).await {
                        tracing::warn!("drain cap reached; exiting with a command still running");
                    }
                    handle.exit(0);
                });
            } else {
                // A second launch just surfaces the existing instance.
                show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--background"]),
        ))
        .plugin(tauri_plugin_opener::init())
        .manage(shell.clone())
        .manage(update::BootInfo::normal())
        .invoke_handler(tauri::generate_handler![
            server_info,
            open_docs,
            install_status,
            install_app,
            uninstall_app,
            rollback_install,
            admin_preview,
            admin_run,
            boot_mode,
            version_notice,
            ack_version_notice
        ])
        .setup(move |app| {
            // The API: `posd`, installed and kept answering. The console binds nothing.
            shell.daemon.start();

            build_tray(app)?;

            // Make the OS autostart entry follow the setting — `posd` writes the file, so
            // watch it rather than a channel.
            let app_settings = current_settings().app;
            sync_autostart(app.handle(), app_settings.start_at_boot);
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let mut last = app_settings.start_at_boot;
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        let now = current_settings().app.start_at_boot;
                        if now != last {
                            last = now;
                            sync_autostart(&handle, now);
                        }
                    }
                });
            }

            let start_hidden = background || app_settings.start_minimized;
            if !start_hidden {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if current_settings().app.minimize_to_tray_on_close {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");
    app.run(move |_, event| {
        if let tauri::RunEvent::Exit = event {
            daemon.shutdown();
        }
    });
}

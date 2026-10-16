//! Ledger POS — desktop shell.
//!
//! The Tauri layer is deliberately thin: all real functionality (settings,
//! providers, REST API) lives in the library so the headless `posd` binary can
//! reuse it. The shell adds: window + tray, minimize-to-tray on close,
//! start-at-boot syncing, single-instance focus, and a couple of UI commands.

pub mod activity;
pub mod admission;
pub mod cli;
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
    state
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
    /// The offer has already been answered once.
    prompt_dismissed: bool,
    /// The version kept for one rollback, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_version: Option<String>,
}

fn install_info(state: &AppState) -> InstallInfo {
    let layout = install::Layout::current();
    InstallInfo {
        available: install::Source::detect().is_some(),
        installed: install::is_installed(&layout),
        installed_path: install::is_installed(&layout)
            .then(|| layout.app_path().display().to_string()),
        prompt_dismissed: state.settings.get().app.install_prompt_dismissed,
        previous_version: install::txn::previous_version(&layout),
    }
}

#[tauri::command]
fn install_status(state: tauri::State<'_, AppState>) -> InstallInfo {
    install_info(&state)
}

#[tauri::command]
fn install_app(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<InstallInfo, String> {
    let _ticket = state.admission.enter().map_err(|_| "updating".to_string())?;
    let source = install::Source::detect()
        .ok_or("this build is not an AppImage, so there is nothing to integrate")?;
    let layout = install::Layout::current();
    install::install(&layout, &source)?;
    let _ = state.settings.update(|s| s.app.install_prompt_dismissed = true);
    // An autostart entry written before the install still points at wherever the
    // AppImage was downloaded; move it to the copy that is now permanent.
    if state.settings.get().app.start_at_boot {
        sync_autostart(&app, true);
    }
    Ok(install_info(&state))
}

#[tauri::command]
fn uninstall_app(state: tauri::State<'_, AppState>) -> Result<InstallInfo, String> {
    let _ticket = state.admission.enter().map_err(|_| "updating".to_string())?;
    install::uninstall(&install::Layout::current())?;
    Ok(install_info(&state))
}

/// `--rollback` from the console: the previous version goes back on the menu and runs from
/// the next start. Refused while `posd` runs, like the command — it may be that version.
#[tauri::command]
fn rollback_install(state: tauri::State<'_, AppState>) -> Result<String, String> {
    use update::system::System;
    let _ticket = state.admission.enter().map_err(|_| "updating".to_string())?;
    let pids = update::system::RealSystem::new().posd_pids();
    if !pids.is_empty() {
        return Err(update::engine::posd_message(&pids));
    }
    install::rollback(&install::Layout::current())
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

/// Remember that the offer was declined, so it is made once and not again.
#[tauri::command]
fn dismiss_install_prompt(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let _ticket = state.admission.enter().map_err(|_| "updating".to_string())?;
    state
        .settings
        .update(|s| s.app.install_prompt_dismissed = true)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------- versions and updates

#[tauri::command]
fn boot_mode(info: tauri::State<'_, update::BootInfo>) -> update::BootInfo {
    info.inner().clone()
}

#[tauri::command]
fn version_notice(state: tauri::State<'_, AppState>) -> Option<runstate::NoticeView> {
    state.run_state.notice()
}

#[tauri::command]
fn ack_version_notice(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let _ticket = state.admission.enter().map_err(|_| "updating".to_string())?;
    state.run_state.ack()
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
        install::rollback(&ctx.layout)?;
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
    running: bool,
    host: String,
    port: u16,
    base_url: String,
    docs_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    version: String,
}

fn ui_server_info(state: &AppState) -> UiServerInfo {
    let status = state.status();
    let settings = state.settings.get();
    let (host, port) = if status.running {
        (status.host.clone(), status.port)
    } else {
        (settings.server.host.clone(), settings.server.port)
    };
    // A wildcard bind address is not a connectable URL.
    let connect_host = if host == "0.0.0.0" || host == "::" { "127.0.0.1".to_string() } else { host.clone() };
    let base_url = format!("http://{connect_host}:{port}");
    UiServerInfo {
        running: status.running,
        host,
        port,
        docs_url: format!("{base_url}/docs"),
        base_url,
        error: status.error,
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

#[tauri::command]
fn server_info(state: tauri::State<'_, AppState>) -> UiServerInfo {
    ui_server_info(&state)
}

#[tauri::command]
fn open_docs(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let info = ui_server_info(&state);
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
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &docs, &sep, &quit])?;

    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().expect("window icon").clone())
        .tooltip("Ledger POS — payment terminal bridge")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "docs" => {
                let state = app.state::<AppState>();
                let url = ui_server_info(&state).docs_url;
                let _ = app.opener().open_url(url, None::<&str>);
            }
            "quit" => {
                // Best effort: give `app.stopping` a moment to reach its subscribers.
                let state = app.state::<AppState>().inner().clone();
                tauri::async_runtime::block_on(events::shutdown(&state, std::time::Duration::from_secs(3)));
                app.exit(0)
            }
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
    let exe = match app.env().appimage {
        // The AppImage path, not the transient /tmp/.mount_* exe inside it.
        Some(appimage) => std::path::PathBuf::from(appimage),
        None => std::env::current_exe()?,
    };
    // Desktop Entry spec: arguments containing spaces must be double-quoted,
    // with the characters that stay special inside quotes backslash-escaped.
    let quoted = format!(
        "\"{}\"",
        exe.display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$")
    );
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        &file,
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Version=1.0\n\
             Name={name}\n\
             Comment={name} — start in the tray at login\n\
             Exec={quoted} --background\n\
             StartupNotify=false\n\
             Terminal=false\n"
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

/// An update screen instead of the app (spec §3.1): no single-instance, no API server, no
/// tray, no events — and no settings loaded, so nothing migrates until the new version
/// runs installed.
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
    let state = build_state();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == update::system::QUIT_FOR_UPDATE) {
                // An update is replacing this copy (spec §3.4): stop admitting changes, let
                // what was admitted finish, announce `app.stopping`, then exit.
                let state = app.state::<AppState>().inner().clone();
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    if !state.admission.drain(std::time::Duration::from_secs(300)).await {
                        tracing::warn!("drain cap reached; exiting with requests still in flight");
                    }
                    events::shutdown(&state, std::time::Duration::from_secs(3)).await;
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
        .manage(state.clone())
        .manage(update::BootInfo::normal())
        .invoke_handler(tauri::generate_handler![
            server_info,
            open_docs,
            install_status,
            install_app,
            uninstall_app,
            dismiss_install_prompt,
            rollback_install,
            admin_preview,
            admin_run,
            boot_mode,
            version_notice,
            ack_version_notice
        ])
        .setup(move |app| {
            // REST API — runs for the app's whole life, window shown or not.
            tauri::async_runtime::spawn(server::run_server(state.clone()));
            // Webhook events: idle unless an administrator configured a subscription.
            tauri::async_runtime::spawn(events::run(state.clone()));

            build_tray(app)?;

            // Make the OS autostart entry follow the setting, now and on change.
            let app_settings = state.settings.get().app;
            sync_autostart(app.handle(), app_settings.start_at_boot);
            {
                let handle = app.handle().clone();
                let mut rx = state.settings.subscribe();
                tauri::async_runtime::spawn(async move {
                    let mut last = rx.borrow().app.start_at_boot;
                    while rx.changed().await.is_ok() {
                        let now = rx.borrow().app.start_at_boot;
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
                let state = window.state::<AppState>();
                if state.settings.get().app.minimize_to_tray_on_close {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

//! Ledger POS — desktop shell.
//!
//! The Tauri layer is deliberately thin: all real functionality (settings,
//! providers, REST API) lives in the library so the headless `posd` binary can
//! reuse it. The shell adds: window + tray, minimize-to-tray on close,
//! start-at-boot syncing, single-instance focus, and a couple of UI commands.

pub mod activity;
pub mod providers;
pub mod server;
pub mod settings;

use std::sync::Arc;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_opener::OpenerExt;

use crate::providers::{saman::SamanProvider, sandbox::SandboxProvider, ProviderRegistry};
use crate::server::AppState;
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
    AppState::new(settings, registry)
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

fn sync_autostart(app: &AppHandle, enabled: bool) {
    let autostart = app.autolaunch();
    let result = if enabled { autostart.enable() } else { autostart.disable() };
    if let Err(e) = result {
        // Disabling when never enabled fails on some platforms — only surface real trouble.
        if enabled {
            tracing::warn!("failed to apply start-at-boot: {e}");
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
    init_tracing();
    tune_webview_env();
    let state = build_state();
    let background = std::env::args().any(|a| a == "--background" || a == "--minimized");

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second launch just surfaces the existing instance.
            show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--background"]),
        ))
        .plugin(tauri_plugin_opener::init())
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![server_info, open_docs])
        .setup(move |app| {
            // REST API — runs for the app's whole life, window shown or not.
            tauri::async_runtime::spawn(server::run_server(state.clone()));

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

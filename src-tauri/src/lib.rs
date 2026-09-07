//! Ledger POS — desktop shell.
//!
//! The Tauri layer is deliberately thin: all real functionality (settings,
//! providers, REST API) lives in the library so the headless `posd` binary can
//! reuse it. The shell adds: window + tray, minimize-to-tray on close,
//! start-at-boot syncing, single-instance focus, and a couple of UI commands.

pub mod activity;
pub mod install;
pub mod providers;
pub mod security;
pub mod server;
pub mod settings;

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
    AppState::new(settings, registry, auth)
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
}

fn install_info(state: &AppState) -> InstallInfo {
    let layout = install::Layout::current();
    InstallInfo {
        available: install::Source::detect().is_some(),
        installed: install::is_installed(&layout),
        installed_path: install::is_installed(&layout)
            .then(|| layout.app_path().display().to_string()),
        prompt_dismissed: state.settings.get().app.install_prompt_dismissed,
    }
}

#[tauri::command]
fn install_status(state: tauri::State<'_, AppState>) -> InstallInfo {
    install_info(&state)
}

#[tauri::command]
fn install_app(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<InstallInfo, String> {
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
    install::uninstall(&install::Layout::current())?;
    Ok(install_info(&state))
}

/// Remember that the offer was declined, so it is made once and not again.
#[tauri::command]
fn dismiss_install_prompt(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state
        .settings
        .update(|s| s.app.install_prompt_dismissed = true)
        .map(|_| ())
        .map_err(|e| e.to_string())
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

/// `--install` / `--uninstall` / `--install-status`, handled before the GUI
/// starts so the same binary can be driven from a provisioning script.
/// Returns true when the process has done its job and should exit.
fn handle_install_cli() -> bool {
    let arg = std::env::args().nth(1);
    let layout = install::Layout::current();
    match arg.as_deref() {
        Some("--install") => {
            match install::Source::detect() {
                None => {
                    eprintln!(
                        "--install only applies to the AppImage build; this copy was started another way."
                    );
                    std::process::exit(2);
                }
                Some(source) => match install::install(&layout, &source) {
                    Ok(report) => {
                        println!("Installed Ledger POS for {}:", whoami());
                        for p in report.written {
                            println!("  {}", p.display());
                        }
                        println!(
                            "\nLauncher: your application menu. Administrator commands: posd admin status"
                        );
                        println!(
                            "If `posd` is not found, add ~/.local/bin to your PATH."
                        );
                    }
                    Err(e) => {
                        eprintln!("install failed: {e}");
                        std::process::exit(1);
                    }
                },
            }
            true
        }
        Some("--uninstall") => {
            match install::uninstall(&layout) {
                Ok(report) if report.removed.is_empty() => println!("Nothing was installed."),
                Ok(report) => {
                    println!("Removed:");
                    for p in report.removed {
                        println!("  {}", p.display());
                    }
                    println!("\nYour settings and the administrator file were left untouched.");
                }
                Err(e) => {
                    eprintln!("uninstall failed: {e}");
                    std::process::exit(1);
                }
            }
            true
        }
        Some("--install-status") => {
            println!("appimage   : {}", install::Source::detect().is_some());
            println!("installed  : {}", install::is_installed(&layout));
            println!("app        : {}", layout.app_path().display());
            println!("desktop    : {}", layout.desktop_path().display());
            println!("posd       : {}", layout.posd_path().display());
            true
        }
        _ => false,
    }
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "this user".into())
}

pub fn run() {
    if handle_install_cli() {
        return;
    }
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
        .invoke_handler(tauri::generate_handler![
            server_info,
            open_docs,
            install_status,
            install_app,
            uninstall_app,
            dismiss_install_prompt
        ])
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

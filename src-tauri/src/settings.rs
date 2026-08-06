//! Persistent, user-editable application settings.
//!
//! Stored as pretty JSON in the platform config directory
//! (e.g. `~/.config/ledger-pos/settings.json`), editable from the UI, the REST
//! API, or a text editor. Changes are broadcast on a watch channel so the HTTP
//! server can rebind and the shell can re-apply autostart without restarting.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;
use utoipa::ToSchema;

pub const DEFAULT_PORT: u16 = 4373;
pub const APP_DIR_NAME: &str = "ledger-pos";

fn d_host() -> String { "127.0.0.1".into() }
fn d_port() -> u16 { DEFAULT_PORT }
fn d_true() -> bool { true }
fn d_cors_origins() -> Vec<String> {
    vec![
        // Tauri webview origins (Linux/Windows use tauri://, Windows also http://tauri.localhost)
        "tauri://localhost".into(),
        "http://tauri.localhost".into(),
        // Nuxt dev server
        "http://localhost:14373".into(),
        "http://127.0.0.1:14373".into(),
    ]
}
fn d_active_provider() -> String { "sandbox".into() }

/// HTTP API settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerSettings {
    /// Interface to bind. Keep 127.0.0.1 unless other machines must reach the API.
    pub host: String,
    /// TCP port for the REST API. Default 4373.
    pub port: u16,
    /// Browser origins allowed by CORS. Non-browser clients (curl, scripts,
    /// desktop apps) are unaffected by CORS either way.
    pub cors_allowed_origins: Vec<String>,
    /// Allow every origin. Convenient, but any web page you visit could then call
    /// this API from your browser — leave off unless you understand that.
    pub cors_allow_all: bool,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            host: d_host(),
            port: d_port(),
            cors_allowed_origins: d_cors_origins(),
            cors_allow_all: false,
        }
    }
}

/// Desktop shell behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct AppBehaviorSettings {
    /// Closing the window hides to the system tray instead of quitting.
    pub minimize_to_tray_on_close: bool,
    /// Launch at login/boot (applied through the OS autostart mechanism).
    pub start_at_boot: bool,
    /// Start with the window hidden (tray only).
    pub start_minimized: bool,
}

impl Default for AppBehaviorSettings {
    fn default() -> Self {
        Self {
            minimize_to_tray_on_close: d_true(),
            start_at_boot: false,
            start_minimized: false,
        }
    }
}

/// Provider selection + per-provider configuration blobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct ProvidersSettings {
    /// The provider used by default (`/api/v1/providers/active` routes).
    pub active: String,
    /// Provider id -> provider-specific configuration object.
    #[schema(value_type = Object)]
    pub configs: serde_json::Map<String, Value>,
}

impl Default for ProvidersSettings {
    fn default() -> Self {
        Self {
            active: d_active_provider(),
            configs: serde_json::Map::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub server: ServerSettings,
    pub app: AppBehaviorSettings,
    pub providers: ProvidersSettings,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("invalid settings: {0}")]
    Invalid(String),
    #[error("failed to persist settings: {0}")]
    Io(#[from] std::io::Error),
}

pub fn validate(s: &Settings) -> Result<(), SettingsError> {
    if s.server.port == 0 {
        return Err(SettingsError::Invalid("server.port must be 1-65535".into()));
    }
    if s.server.host.trim().is_empty() {
        return Err(SettingsError::Invalid("server.host must not be empty".into()));
    }
    if s.providers.active.trim().is_empty() {
        return Err(SettingsError::Invalid("providers.active must not be empty".into()));
    }
    Ok(())
}

pub fn settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR_NAME)
        .join("settings.json")
}

pub struct SettingsStore {
    path: PathBuf,
    tx: watch::Sender<Settings>,
}

impl SettingsStore {
    /// Load settings from disk (writing defaults on first run or unreadable file).
    pub fn load() -> Self {
        let path = settings_path();
        let settings = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<Settings>(&raw) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("settings file unreadable ({e}); keeping it and using defaults");
                    // Preserve the broken file for the user instead of overwriting it.
                    let backup = path.with_extension("json.invalid");
                    let _ = std::fs::copy(&path, &backup);
                    Settings::default()
                }
            },
            Err(_) => {
                let s = Settings::default();
                let _ = persist(&path, &s);
                s
            }
        };
        let (tx, _) = watch::channel(settings);
        Self { path, tx }
    }

    pub fn get(&self) -> Settings {
        self.tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Settings> {
        self.tx.subscribe()
    }

    /// Apply a mutation, validate, persist, and broadcast. Returns the new settings.
    pub fn update(
        &self,
        mutate: impl FnOnce(&mut Settings),
    ) -> Result<Settings, SettingsError> {
        let mut next = self.get();
        mutate(&mut next);
        validate(&next)?;
        persist(&self.path, &next)?;
        self.tx.send_replace(next.clone());
        Ok(next)
    }
}

fn persist(path: &PathBuf, s: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(s).expect("settings serialize");
    // Write-then-rename so a crash mid-write can't truncate the settings file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

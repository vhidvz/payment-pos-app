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

use crate::migrate::{
    commit_upgrade, keep_invalid, newer_message, newer_on_disk, read_and_upgrade, record, stamp,
    FileStatus, Loaded, MigrateLock, MigrationRecord, Outcome, Step,
};

pub const SETTINGS_SCHEMA: u32 = 1;
/// 0 → 1: 0.1.0's file, unchanged in shape; only the version is stamped.
pub const SETTINGS_STEPS: &[Step] = &[Step { to: 1, apply: crate::migrate::noop }];

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
    /// Set once the desktop-integration offer has been answered, so an AppImage
    /// run that the user chose not to integrate does not ask again at every launch.
    pub install_prompt_dismissed: bool,
}

impl Default for AppBehaviorSettings {
    fn default() -> Self {
        Self {
            minimize_to_tray_on_close: d_true(),
            start_at_boot: false,
            start_minimized: false,
            install_prompt_dismissed: false,
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
    /// The file belongs to a newer version: never written (spec §2.3).
    #[error("{0}")]
    ReadOnly(String),
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

/// The settings as they are on disk, for a process that must never write them — the
/// console, while `posd` owns the file. Lenient: a missing, unreadable or newer file reads
/// as what serde can make of it, else defaults; nothing is migrated, backed up or created.
pub fn peek(path: &std::path::Path) -> Settings {
    let Ok(raw) = std::fs::read(path) else { return Settings::default() };
    let Ok(mut doc) = serde_json::from_slice::<Value>(&raw) else { return Settings::default() };
    crate::migrate::strip(&mut doc);
    serde_json::from_value(doc).unwrap_or_default()
}

pub struct SettingsStore {
    path: PathBuf,
    tx: watch::Sender<Settings>,
    status: std::sync::Mutex<FileStatus>,
    /// Why writes are refused, when the file belongs to a newer version — set at load, or
    /// later when a write finds a newer version migrated the file underneath this process.
    read_only: std::sync::Mutex<Option<String>>,
    migrations: Vec<MigrationRecord>,
}

impl SettingsStore {
    /// Load settings from disk (writing defaults on first run).
    pub fn load() -> Self {
        Self::at(settings_path())
    }

    /// Same, for an explicit path — the seam tests use so they never touch the
    /// real configuration file. Migrates under the migrate lock (spec §2.2); a newer
    /// file is read leniently and never written (§2.3).
    pub fn at(path: PathBuf) -> Self {
        let dir = path.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let _lock = MigrateLock::acquire(&dir)
            .map_err(|e| tracing::warn!("cannot take the migrate lock in {} ({e})", dir.display()))
            .ok();
        let mut migrations = Vec::new();
        let (settings, status, read_only) = match read_and_upgrade(&path, SETTINGS_STEPS) {
            Loaded::Missing => {
                let s = Settings::default();
                let _ = persist(&path, &s);
                (s, FileStatus::Ok, None)
            }
            Loaded::Ready { doc, outcome } => match serde_json::from_value::<Settings>(doc) {
                Ok(s) => {
                    if let Outcome::Upgraded { from, to } = outcome {
                        match commit_upgrade(&path, from, &render(&s)) {
                            Ok(backup) => migrations.push(record(&path, from, to, Some(&backup))),
                            Err(e) => tracing::warn!("cannot write migrated settings ({e}); using them in memory"),
                        }
                    }
                    (s, FileStatus::Ok, None)
                }
                Err(e) => recovered(&path, &e.to_string()),
            },
            Loaded::Newer { doc, found, supported } => {
                let why = newer_message(&path, found, supported);
                tracing::warn!("{why}; settings are read-only this run");
                let s = serde_json::from_value::<Settings>(doc).unwrap_or_default();
                (s, FileStatus::ReadOnlyNewer, Some(why))
            }
            Loaded::Broken(e) => recovered(&path, &e),
        };
        let (tx, _) = watch::channel(settings);
        Self {
            path,
            tx,
            status: std::sync::Mutex::new(status),
            read_only: std::sync::Mutex::new(read_only),
            migrations,
        }
    }

    pub fn status(&self) -> FileStatus {
        *self.status.lock().expect("settings status")
    }

    /// From now on this file belongs to a newer version: refuse every write.
    fn mark_read_only(&self, found: u32) -> String {
        let why = newer_message(&self.path, found, SETTINGS_SCHEMA);
        tracing::warn!("{why}; settings are read-only from now on");
        *self.read_only.lock().expect("settings read-only") = Some(why.clone());
        *self.status.lock().expect("settings status") = FileStatus::ReadOnlyNewer;
        why
    }

    /// The migrations this store performed while loading.
    pub fn migrations(&self) -> Vec<MigrationRecord> {
        self.migrations.clone()
    }

    /// Refuse when the file belongs to a newer version — as loaded, or as it is on disk now.
    pub fn ensure_writable(&self) -> Result<(), SettingsError> {
        if let Some(why) = self.read_only.lock().expect("settings read-only").clone() {
            return Err(SettingsError::ReadOnly(why));
        }
        if let Some(found) = newer_on_disk(&self.path, SETTINGS_SCHEMA) {
            return Err(SettingsError::ReadOnly(self.mark_read_only(found)));
        }
        Ok(())
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
        self.ensure_writable()?;
        let mut next = self.get();
        mutate(&mut next);
        validate(&next)?;
        // Check again under the lock a newer version would migrate under, then write.
        let dir = self.path.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let _lock = MigrateLock::acquire(&dir).ok();
        if let Some(found) = newer_on_disk(&self.path, SETTINGS_SCHEMA) {
            return Err(SettingsError::ReadOnly(self.mark_read_only(found)));
        }
        persist(&self.path, &next)?;
        self.tx.send_replace(next.clone());
        Ok(next)
    }
}

/// Today's unreadable-file behaviour: keep it as `.invalid`, run on defaults.
fn recovered(path: &std::path::Path, why: &str) -> (Settings, FileStatus, Option<String>) {
    tracing::warn!("settings file unreadable ({why}); keeping it as .invalid and using defaults");
    keep_invalid(path);
    (Settings::default(), FileStatus::Recovered, None)
}

fn render(s: &Settings) -> Vec<u8> {
    let mut doc = serde_json::to_value(s).expect("settings serialize");
    stamp(&mut doc, SETTINGS_SCHEMA).expect("settings serialize to an object");
    serde_json::to_vec_pretty(&doc).expect("settings serialize")
}

/// Write-then-rename (with fsync) so a crash mid-write can't truncate the settings file.
fn persist(path: &PathBuf, s: &Settings) -> std::io::Result<()> {
    let meta = std::fs::metadata(path).ok();
    crate::fsutil::atomic_write(path, &render(s), meta.as_ref(), 0o644)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::{assert_contiguous, FileStatus, SCHEMA_KEY};

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ledger-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A settings file exactly as 0.1.0 wrote it: no schema key.
    const V0: &str = r#"{
      "server": { "host": "127.0.0.1", "port": 4999, "corsAllowedOrigins": [], "corsAllowAll": false },
      "app": { "minimizeToTrayOnClose": true, "startAtBoot": true, "startMinimized": false, "installPromptDismissed": true },
      "providers": { "active": "saman-ssp1126", "configs": { "saman-ssp1126": { "transport": "tcp", "host": "192.168.1.198", "port": 1197 } } }
    }"#;

    #[test]
    fn the_chain_is_contiguous() {
        assert_contiguous(SETTINGS_STEPS);
    }

    #[test]
    fn a_v0_file_is_migrated_with_every_value_kept_and_a_backup() {
        let d = dir();
        let p = d.join("settings.json");
        std::fs::write(&p, V0).unwrap();

        let store = SettingsStore::at(p.clone());
        let s = store.get();
        assert_eq!(s.server.port, 4999);
        assert!(s.app.start_at_boot);
        assert_eq!(s.providers.active, "saman-ssp1126");
        assert_eq!(s.providers.configs["saman-ssp1126"]["host"], "192.168.1.198");

        let on_disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(on_disk[SCHEMA_KEY], SETTINGS_SCHEMA);
        assert_eq!(std::fs::read_to_string(d.join("settings.json.v0.bak")).unwrap(), V0);
        let m = store.migrations();
        assert_eq!((m.len(), m[0].from, m[0].to), (1, 0, 1));
        assert_eq!(store.status(), FileStatus::Ok);
    }

    /// A crash after the backup but before the write: the next start migrates again and
    /// never overwrites the original backup.
    #[test]
    fn a_replayed_migration_keeps_the_first_backup() {
        let d = dir();
        let p = d.join("settings.json");
        std::fs::write(d.join("settings.json.v0.bak"), "the original").unwrap();
        std::fs::write(&p, V0).unwrap();
        let _ = SettingsStore::at(p.clone());
        assert_eq!(std::fs::read_to_string(d.join("settings.json.v0.bak")).unwrap(), "the original");
    }

    #[test]
    fn a_newer_file_is_read_and_never_written() {
        let d = dir();
        let p = d.join("settings.json");
        let newer = V0.replacen('{', r#"{ "schemaVersion": 9, "futureField": [1,2,3],"#, 1);
        std::fs::write(&p, &newer).unwrap();

        let store = SettingsStore::at(p.clone());
        assert_eq!(store.get().server.port, 4999, "known fields read leniently");
        assert_eq!(store.status(), FileStatus::ReadOnlyNewer);
        assert!(matches!(store.update(|s| s.server.port = 1), Err(SettingsError::ReadOnly(_))));
        assert!(matches!(store.ensure_writable(), Err(SettingsError::ReadOnly(_))));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), newer, "byte-identical");
    }

    #[test]
    fn an_invalid_version_is_recovered_not_read_as_zero() {
        let d = dir();
        let p = d.join("settings.json");
        let bad = V0.replacen('{', r#"{ "schemaVersion": "1","#, 1);
        std::fs::write(&p, &bad).unwrap();
        let store = SettingsStore::at(p.clone());
        assert_eq!(store.status(), FileStatus::Recovered);
        assert_eq!(store.get(), Settings::default());
        assert_eq!(std::fs::read_to_string(d.join("settings.json.invalid")).unwrap(), bad);
    }

    #[test]
    fn every_write_carries_the_schema() {
        let d = dir();
        let p = d.join("settings.json");
        let store = SettingsStore::at(p.clone());
        store.update(|s| s.server.port = 5000).unwrap();
        let on_disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(on_disk[SCHEMA_KEY], SETTINGS_SCHEMA);
        assert_eq!(on_disk["server"]["port"], 5000);
    }

    /// A newer version may migrate the file after this process loaded it (a package
    /// upgrade while this copy runs in the tray): the next write must notice, not clobber.
    #[test]
    fn a_file_a_newer_version_migrated_after_load_is_never_overwritten() {
        let d = dir();
        let p = d.join("settings.json");
        let store = SettingsStore::at(p.clone());
        assert_eq!(store.status(), FileStatus::Ok);
        let newer = r#"{"schemaVersion":9,"server":{"port":4999}}"#;
        std::fs::write(&p, newer).unwrap();

        assert!(matches!(store.update(|s| s.server.port = 1), Err(SettingsError::ReadOnly(_))));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), newer, "byte-identical");
        assert_eq!(store.status(), FileStatus::ReadOnlyNewer);
        assert!(matches!(store.ensure_writable(), Err(SettingsError::ReadOnly(_))));
    }

    /// The API body has no schema key, before or after.
    /// The console reads settings and never writes them: `posd` alone owns the file.
    #[test]
    fn peek_reads_without_writing() {
        let d = dir();
        let missing = d.join("none.json");
        assert_eq!(peek(&missing), Settings::default());
        assert!(!missing.exists(), "peek must not create the file");

        let p = d.join("settings.json");
        std::fs::write(&p, V0).unwrap();
        let s = peek(&p);
        assert_eq!(s.server.port, 4999);
        assert!(s.app.start_at_boot);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), V0, "no migration written");
        assert!(!d.join("settings.json.v0.bak").exists(), "no backup made");
    }

    #[test]
    fn peek_survives_garbage() {
        let d = dir();
        let p = d.join("settings.json");
        std::fs::write(&p, b"{ not json").unwrap();
        assert_eq!(peek(&p), Settings::default());
        assert!(p.exists() && !d.join("settings.json.invalid").exists(), "left exactly as found");
    }

    #[test]
    fn the_schema_key_never_reaches_the_model() {
        let d = dir();
        let p = d.join("settings.json");
        std::fs::write(&p, V0).unwrap();
        let v = serde_json::to_value(SettingsStore::at(p).get()).unwrap();
        assert!(v.get(SCHEMA_KEY).is_none());
    }
}

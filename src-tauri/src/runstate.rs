//! `state.json` — run state that is not a setting (spec §4).
//!
//! - `app` is written **only by the desktop app**: the last version that ran and the
//!   pending "updated" notice. `posd` never reads or writes it, so an older daemon cannot
//!   make the app's version appear to move backwards.
//! - `migrations` is appended by whichever process migrates a file — the migrations not
//!   yet shown to anyone — under the migrate lock.
//! - A newer-schema file is read-only and stays byte-identical.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::migrate::{read_and_upgrade, FileStatus, Loaded, MigrateLock, MigrationRecord, Step};

pub const STATE_SCHEMA: u32 = 1;
pub const STATE_STEPS: &[Step] = &[Step { to: 1, apply: crate::migrate::noop }];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum NoticeKind {
    Upgraded,
    Downgraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub kind: NoticeKind,
    /// `None`: the data came from before 1.0, whose version nobody recorded.
    pub from: Option<String>,
    pub to: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRun {
    pub last_run_version: String,
    #[serde(default)]
    pub pending_notice: Option<Notice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunState {
    pub app: Option<AppRun>,
    pub migrations: Vec<MigrationRecord>,
}

/// What the UI shows once: a version change, the migrations not yet seen, or both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoticeView {
    pub notice: Option<Notice>,
    pub migrations: Vec<MigrationRecord>,
}

pub fn state_path() -> PathBuf {
    crate::settings::settings_path().with_file_name("state.json")
}

/// What an app start does to `app` — spec §4's table, pure.
pub fn next_app_run(prev: Option<&AppRun>, has_migrations: bool, current: &str, now: &str) -> AppRun {
    let notice = |kind, from: Option<String>| Some(Notice { kind, from, to: current.to_string(), at: now.to_string() });
    match prev {
        None if !has_migrations => AppRun { last_run_version: current.into(), pending_notice: None },
        None => AppRun { last_run_version: current.into(), pending_notice: notice(NoticeKind::Upgraded, None) },
        Some(p) if p.last_run_version == current => p.clone(),
        Some(p) => {
            let from = match &p.pending_notice {
                Some(n) => n.from.clone(),
                None => Some(p.last_run_version.clone()),
            };
            if from.as_deref() == Some(current) {
                // Went away and came back before anyone looked: nothing to announce.
                return AppRun { last_run_version: current.into(), pending_notice: None };
            }
            let newer = |a: &str, b: &str| match (semver::Version::parse(a), semver::Version::parse(b)) {
                (Ok(a), Ok(b)) => a > b,
                _ => false,
            };
            let kind = match from.as_deref() {
                Some(f) if newer(f, current) => NoticeKind::Downgraded,
                _ => NoticeKind::Upgraded,
            };
            AppRun { last_run_version: current.into(), pending_notice: notice(kind, from) }
        }
    }
}

pub struct StateStore {
    path: Option<PathBuf>,
    inner: Mutex<RunState>,
    writable: bool,
    status: FileStatus,
}

impl StateStore {
    /// Tests and one-shot tools: nothing on disk.
    pub fn in_memory() -> Self {
        Self { path: None, inner: Mutex::new(RunState::default()), writable: true, status: FileStatus::Ok }
    }

    pub fn open(path: PathBuf) -> Self {
        let _lock = Self::lock(&path);
        let (state, writable, status) = Self::read(&path);
        Self { path: Some(path), inner: Mutex::new(state), writable, status }
    }

    fn lock(path: &std::path::Path) -> Option<MigrateLock> {
        let dir = path.parent().unwrap_or(std::path::Path::new("."));
        MigrateLock::acquire(dir).ok()
    }

    fn read(path: &std::path::Path) -> (RunState, bool, FileStatus) {
        let recovered = |why: String| {
            tracing::warn!("run state {} unreadable ({why}); keeping it as .invalid", path.display());
            crate::migrate::keep_invalid(path);
            (RunState::default(), true, FileStatus::Recovered)
        };
        match read_and_upgrade(path, STATE_STEPS) {
            Loaded::Missing => (RunState::default(), true, FileStatus::Ok),
            Loaded::Ready { doc, .. } => match serde_json::from_value(doc) {
                Ok(s) => (s, true, FileStatus::Ok),
                Err(e) => recovered(e.to_string()),
            },
            Loaded::Newer { doc, .. } => {
                (serde_json::from_value(doc).unwrap_or_default(), false, FileStatus::ReadOnlyNewer)
            }
            Loaded::Broken(e) => recovered(e),
        }
    }

    /// Read-modify-write under the migrate lock, re-reading first so another process's
    /// append is never lost. A read-only store changes nothing, on disk or in memory.
    fn mutate(&self, change: impl FnOnce(&mut RunState)) -> Result<(), String> {
        if !self.writable {
            return Err("state_read_only".into());
        }
        let mut inner = self.inner.lock().expect("run state");
        let Some(path) = &self.path else {
            change(&mut inner);
            return Ok(());
        };
        let _lock = Self::lock(path);
        let (mut fresh, writable, _) = Self::read(path);
        if !writable {
            return Err("state_read_only".into());
        }
        change(&mut fresh);
        let mut doc = serde_json::to_value(&fresh).map_err(|e| e.to_string())?;
        crate::migrate::stamp(&mut doc, STATE_SCHEMA).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
        let meta = std::fs::metadata(path).ok();
        crate::fsutil::atomic_write(path, &bytes, meta.as_ref(), 0o644).map_err(|e| e.to_string())?;
        *inner = fresh;
        Ok(())
    }

    pub fn record_migrations(&self, records: &[MigrationRecord]) {
        if records.is_empty() {
            return;
        }
        if let Err(e) = self.mutate(|s| s.migrations.extend_from_slice(records)) {
            for r in records {
                tracing::info!("migrated {} {}→{} (not recorded: {e})", r.file, r.from, r.to);
            }
        }
    }

    /// The desktop app's start (spec §4). `posd` never calls this.
    pub fn note_app_start(&self, current: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        let _ = self.mutate(|s| {
            let has = !s.migrations.is_empty();
            s.app = Some(next_app_run(s.app.as_ref(), has, current, &now));
        });
    }

    pub fn notice(&self) -> Option<NoticeView> {
        let s = self.inner.lock().expect("run state");
        let notice = s.app.as_ref().and_then(|a| a.pending_notice.clone());
        (notice.is_some() || !s.migrations.is_empty())
            .then(|| NoticeView { notice, migrations: s.migrations.clone() })
    }

    /// The UI has shown it: clear the notice and the migrations in one write.
    pub fn ack(&self) -> Result<(), String> {
        self.mutate(|s| {
            if let Some(app) = &mut s.app {
                app.pending_notice = None;
            }
            s.migrations.clear();
        })
    }

    pub fn status(&self) -> FileStatus {
        self.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(file: &str) -> MigrationRecord {
        MigrationRecord { file: file.into(), from: 0, to: 1, backup: None, by: "posd 1.0.0".into(), at: "t".into() }
    }
    fn path() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ledger-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("state.json")
    }

    #[test]
    fn the_chain_is_contiguous() {
        crate::migrate::assert_contiguous(STATE_STEPS);
    }

    // ------------------------------------------------ the pure table (spec §4)

    #[test]
    fn first_run_with_nothing_migrated_has_no_notice() {
        let run = next_app_run(None, false, "1.0.0", "t");
        assert_eq!(run, AppRun { last_run_version: "1.0.0".into(), pending_notice: None });
    }

    #[test]
    fn pre_1_0_data_that_was_migrated_gives_a_notice_from_unknown() {
        let n = next_app_run(None, true, "1.0.0", "t").pending_notice.unwrap();
        assert_eq!((n.kind, n.from, n.to), (NoticeKind::Upgraded, None, "1.0.0".into()));
    }

    #[test]
    fn a_version_change_gives_a_notice() {
        let prev = AppRun { last_run_version: "1.0.0".into(), pending_notice: None };
        let n = next_app_run(Some(&prev), false, "1.1.0", "t").pending_notice.unwrap();
        assert_eq!((n.kind, n.from.as_deref()), (NoticeKind::Upgraded, Some("1.0.0")));
    }

    #[test]
    fn an_unacknowledged_notice_keeps_its_earliest_from() {
        let prev = AppRun {
            last_run_version: "1.1.0".into(),
            pending_notice: Some(Notice {
                kind: NoticeKind::Upgraded,
                from: Some("1.0.0".into()),
                to: "1.1.0".into(),
                at: "t0".into(),
            }),
        };
        let n = next_app_run(Some(&prev), false, "1.2.0", "t1").pending_notice.unwrap();
        assert_eq!((n.from.as_deref(), n.to.as_str()), (Some("1.0.0"), "1.2.0"));
    }

    #[test]
    fn going_back_is_a_downgrade_notice() {
        let prev = AppRun { last_run_version: "1.1.0".into(), pending_notice: None };
        let n = next_app_run(Some(&prev), false, "1.0.0", "t").pending_notice.unwrap();
        assert_eq!(n.kind, NoticeKind::Downgraded);
    }

    #[test]
    fn the_same_version_changes_nothing() {
        let prev = AppRun { last_run_version: "1.0.0".into(), pending_notice: None };
        assert_eq!(next_app_run(Some(&prev), true, "1.0.0", "t"), prev);
    }

    // ------------------------------------------------------------ the store

    #[test]
    fn the_notice_survives_a_restart_until_acknowledged() {
        let p = path();
        let s = StateStore::open(p.clone());
        s.record_migrations(&[rec("settings.json")]);
        s.note_app_start("1.0.0");
        assert!(StateStore::open(p.clone()).notice().is_some(), "after a restart");
        StateStore::open(p.clone()).ack().unwrap();
        assert!(StateStore::open(p).notice().is_none());
    }

    /// `posd` appends migrations but never touches `app` (spec §4 ownership).
    #[test]
    fn a_posd_start_never_writes_the_app_section() {
        let p = path();
        StateStore::open(p.clone()).note_app_start("1.1.0");
        let posd = StateStore::open(p.clone());
        posd.record_migrations(&[rec("settings.json")]);
        let on_disk: RunState = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(on_disk.app.unwrap().last_run_version, "1.1.0");
        assert_eq!(on_disk.migrations.len(), 1);
    }

    /// Two processes appending at once: the read-modify-write under the lock loses neither.
    #[test]
    fn concurrent_appends_lose_nothing() {
        let p = path();
        let threads: Vec<_> = (0..4)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || StateStore::open(p).record_migrations(&[rec(&format!("f{i}"))]))
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let on_disk: RunState = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(on_disk.migrations.len(), 4);
    }

    #[test]
    fn a_newer_state_file_stays_byte_identical() {
        let p = path();
        let newer = r#"{"schemaVersion":4,"app":{"lastRunVersion":"9.0.0"}}"#;
        std::fs::write(&p, newer).unwrap();
        let s = StateStore::open(p.clone());
        assert_eq!(s.status(), FileStatus::ReadOnlyNewer);
        s.record_migrations(&[rec("settings.json")]);
        s.note_app_start("1.0.0");
        assert_eq!(s.ack(), Err("state_read_only".to_string()));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), newer);
    }
}

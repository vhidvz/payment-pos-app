//! Schema versions for every file this app persists, and the steps between them.
//!
//! **Contract for every step, now and in later versions** — it is what makes an older
//! app's lenient read of a newer file sound:
//! - pure and deterministic, so a crash mid-migration replays to the same result;
//! - may add fields and rename fields;
//! - **never changes the meaning of an existing field in place** — rename it instead, so
//!   an older version sees the field as absent and falls back to its default.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

pub const SCHEMA_KEY: &str = "schemaVersion";

pub struct Step {
    pub to: u32,
    pub apply: fn(&mut Value) -> Result<(), String>,
}

/// The step of a file whose shape did not change: only the version is stamped.
pub fn noop(_: &mut Value) -> Result<(), String> {
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Current,
    Upgraded { from: u32, to: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateError {
    Newer { found: u32, supported: u32 },
    Invalid(String),
    Step { to: u32, reason: String },
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Newer { found, supported } => write!(
                f,
                "written by a newer Ledger POS (schema {found}; this version understands {supported})"
            ),
            Self::Invalid(why) => write!(f, "invalid: {why}"),
            Self::Step { to, reason } => write!(f, "migration to schema {to} failed: {reason}"),
        }
    }
}

pub fn latest(steps: &[Step]) -> u32 {
    steps.last().map_or(0, |s| s.to)
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The version a document declares. Absent ⇒ 0. Anything but a non-negative integer that
/// fits `u32` ⇒ `Invalid` — never silently 0. An array root is schema 0 (the outbox's v0).
pub fn schema_of(doc: &Value) -> Result<u32, MigrateError> {
    match doc {
        Value::Object(map) => match map.get(SCHEMA_KEY) {
            None => Ok(0),
            Some(v) => v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| {
                MigrateError::Invalid(format!("{SCHEMA_KEY} must be a non-negative integer, found {v}"))
            }),
        },
        Value::Array(_) => Ok(0),
        other => Err(MigrateError::Invalid(format!("expected a JSON object, found {}", kind(other)))),
    }
}

pub fn stamp(doc: &mut Value, version: u32) -> Result<(), MigrateError> {
    match doc.as_object_mut() {
        Some(map) => {
            map.insert(SCHEMA_KEY.into(), version.into());
            Ok(())
        }
        None => Err(MigrateError::Invalid(format!("cannot stamp {}", kind(doc)))),
    }
}

pub fn strip(doc: &mut Value) {
    if let Some(map) = doc.as_object_mut() {
        map.remove(SCHEMA_KEY);
    }
}

pub fn upgrade(doc: &mut Value, steps: &[Step]) -> Result<Outcome, MigrateError> {
    let supported = latest(steps);
    let from = schema_of(doc)?;
    if from > supported {
        return Err(MigrateError::Newer { found: from, supported });
    }
    if from == supported {
        return Ok(Outcome::Current);
    }
    for step in steps.iter().filter(|s| s.to > from) {
        (step.apply)(doc).map_err(|reason| MigrateError::Step { to: step.to, reason })?;
        stamp(doc, step.to)?;
    }
    Ok(Outcome::Upgraded { from, to: supported })
}

/// For each file's own test: the chain has no gap and no repeat.
pub fn assert_contiguous(steps: &[Step]) {
    for (i, s) in steps.iter().enumerate() {
        let want = i as u32 + 1;
        assert_eq!(s.to, want, "step {i} must migrate to {want}");
    }
}

/// How a data file stands this run — what `GET /api/v1/system` reports per file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum FileStatus {
    Ok,
    /// Written by a newer version: read leniently, never written.
    ReadOnlyNewer,
    /// Unreadable or failed to migrate: kept as `<file>.invalid`, defaults in use.
    Recovered,
    /// The administrator file is newer: the console stays locked.
    FailClosed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationRecord {
    pub file: String,
    pub from: u32,
    pub to: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    pub by: String,
    pub at: String,
}

pub fn record(path: &Path, from: u32, to: u32, backup: Option<&Path>) -> MigrationRecord {
    MigrationRecord {
        file: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        from,
        to,
        backup: backup.map(|b| b.display().to_string()),
        by: crate::version::process_label(),
        at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Exclusive across processes for read → upgrade → backup → write: the app and `posd`
/// starting together each migrate under it, and the second re-reads to find the work
/// done. Released on drop (or by the OS when the process dies).
pub struct MigrateLock {
    _file: std::fs::File,
}

impl MigrateLock {
    pub fn acquire(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(".migrate.lock"))?;
        file.lock()?;
        Ok(Self { _file: file })
    }
}

pub enum Loaded {
    Missing,
    /// Upgraded in memory; `doc` has the key stripped.
    Ready { doc: Value, outcome: Outcome },
    /// `doc` has the key stripped, for a lenient read.
    Newer { doc: Value, found: u32, supported: u32 },
    Broken(String),
}

/// Read, parse and upgrade `path` in memory. Writes nothing.
pub fn read_and_upgrade(path: &Path, steps: &[Step]) -> Loaded {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
        Err(e) => return Loaded::Broken(e.to_string()),
    };
    let mut doc: Value = match serde_json::from_slice(&raw) {
        Ok(doc) => doc,
        Err(e) => return Loaded::Broken(e.to_string()),
    };
    match upgrade(&mut doc, steps) {
        Ok(outcome) => {
            strip(&mut doc);
            Loaded::Ready { doc, outcome }
        }
        Err(MigrateError::Newer { found, supported }) => {
            strip(&mut doc);
            Loaded::Newer { doc, found, supported }
        }
        Err(e) => Loaded::Broken(e.to_string()),
    }
}

pub fn backup_path(path: &Path, from: u32) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".v{from}.bak"));
    PathBuf::from(s)
}

/// The schema the file on disk declares right now, when it is newer than `supported` —
/// the write-time half of "never write over a newer file": a newer version may have
/// migrated the file after this process loaded it (a package upgrade while this copy
/// keeps running). Call it under the migrate lock, just before writing.
pub fn newer_on_disk(path: &Path, supported: u32) -> Option<u32> {
    let raw = std::fs::read(path).ok()?;
    let doc: Value = serde_json::from_slice(&raw).ok()?;
    schema_of(&doc).ok().filter(|found| *found > supported)
}

pub fn newer_message(path: &Path, found: u32, supported: u32) -> String {
    format!(
        "{} was written by a newer Ledger POS (schema {found}; this version understands {supported}) \
         — open that version, or restore a .bak backup beside it",
        path.display()
    )
}

/// Persist a migrated document: back the original up once, then replace it.
pub fn commit_upgrade(path: &Path, from: u32, migrated: &[u8]) -> std::io::Result<PathBuf> {
    let backup = backup_path(path, from);
    crate::fsutil::backup_once(path, &backup)?;
    let meta = std::fs::metadata(path).ok();
    crate::fsutil::atomic_write(path, migrated, meta.as_ref(), 0o644)?;
    Ok(backup)
}

/// Today's "unreadable" path, unchanged: keep the file aside as `<file>.invalid`.
pub fn keep_invalid(path: &Path) {
    let mut s = path.as_os_str().to_owned();
    s.push(".invalid");
    let _ = std::fs::copy(path, PathBuf::from(s));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wrap(doc: &mut Value) -> Result<(), String> {
        let v = doc.take();
        *doc = json!({ "inner": v });
        Ok(())
    }
    fn fail(_: &mut Value) -> Result<(), String> {
        Err("cannot".into())
    }
    const TWO: &[Step] = &[Step { to: 1, apply: noop }, Step { to: 2, apply: wrap }];

    #[test]
    fn an_absent_version_is_zero() {
        assert_eq!(schema_of(&json!({ "a": 1 })), Ok(0));
        assert_eq!(schema_of(&json!([1, 2])), Ok(0));
    }

    #[test]
    fn an_invalid_version_is_never_read_as_zero() {
        for bad in [json!("1"), json!(-1), json!(1.5), json!(1.0), json!(null), json!(u64::MAX)] {
            let doc = json!({ SCHEMA_KEY: bad });
            assert!(matches!(schema_of(&doc), Err(MigrateError::Invalid(_))), "{doc}");
        }
        assert!(matches!(schema_of(&json!("text")), Err(MigrateError::Invalid(_))));
    }

    #[test]
    fn steps_run_in_order_and_stamp_each_version() {
        let mut doc = json!({ "a": 1 });
        assert_eq!(upgrade(&mut doc, TWO), Ok(Outcome::Upgraded { from: 0, to: 2 }));
        assert_eq!(doc, json!({ "inner": { "a": 1, SCHEMA_KEY: 1 }, SCHEMA_KEY: 2 }));
    }

    #[test]
    fn only_the_missing_steps_run() {
        let mut doc = json!({ "a": 1, SCHEMA_KEY: 1 });
        assert_eq!(upgrade(&mut doc, TWO), Ok(Outcome::Upgraded { from: 1, to: 2 }));
        assert_eq!(doc["inner"]["a"], 1);
    }

    #[test]
    fn a_current_document_is_left_alone() {
        let mut doc = json!({ "a": 1, SCHEMA_KEY: 2 });
        let before = doc.clone();
        assert_eq!(upgrade(&mut doc, TWO), Ok(Outcome::Current));
        assert_eq!(doc, before);
    }

    #[test]
    fn a_newer_document_is_reported_not_touched() {
        let mut doc = json!({ "a": 1, SCHEMA_KEY: 9 });
        let before = doc.clone();
        assert_eq!(upgrade(&mut doc, TWO), Err(MigrateError::Newer { found: 9, supported: 2 }));
        assert_eq!(doc, before);
    }

    #[test]
    fn a_failing_step_is_reported_with_its_target() {
        let steps: &[Step] = &[Step { to: 1, apply: fail }];
        let mut doc = json!({});
        assert_eq!(upgrade(&mut doc, steps), Err(MigrateError::Step { to: 1, reason: "cannot".into() }));
    }

    #[test]
    #[should_panic(expected = "must migrate to 2")]
    fn a_gap_in_a_chain_is_caught() {
        assert_contiguous(&[Step { to: 1, apply: noop }, Step { to: 3, apply: noop }]);
    }

    #[test]
    fn strip_removes_only_the_key() {
        let mut doc = json!({ "a": 1, SCHEMA_KEY: 2 });
        strip(&mut doc);
        assert_eq!(doc, json!({ "a": 1 }));
    }

    #[test]
    fn backup_path_appends_the_old_version() {
        assert_eq!(
            backup_path(Path::new("/x/settings.json"), 0),
            PathBuf::from("/x/settings.json.v0.bak")
        );
    }

    /// Two migrators: the second waits for the first and then sees its work.
    #[test]
    fn the_lock_serializes_two_migrators() {
        let dir = std::env::temp_dir().join(format!("ledger-lock-{}", uuid::Uuid::new_v4()));
        let held = MigrateLock::acquire(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let d2 = dir.clone();
        let t = std::thread::spawn(move || {
            let _second = MigrateLock::acquire(&d2).unwrap();
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "the second migrator must wait while the first holds the lock"
        );
        drop(held);
        rx.recv_timeout(std::time::Duration::from_secs(5)).expect("acquired after release");
        t.join().unwrap();
    }

    #[test]
    fn read_and_upgrade_classifies_files() {
        let dir = std::env::temp_dir().join(format!("ledger-read-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.json");
        assert!(matches!(read_and_upgrade(&p, TWO), Loaded::Missing));
        std::fs::write(&p, "not json").unwrap();
        assert!(matches!(read_and_upgrade(&p, TWO), Loaded::Broken(_)));
        std::fs::write(&p, r#"{"schemaVersion":7}"#).unwrap();
        assert!(matches!(read_and_upgrade(&p, TWO), Loaded::Newer { found: 7, supported: 2, .. }));
        std::fs::write(&p, r#"{"a":1}"#).unwrap();
        match read_and_upgrade(&p, TWO) {
            Loaded::Ready { doc, outcome } => {
                assert_eq!(outcome, Outcome::Upgraded { from: 0, to: 2 });
                assert!(doc.get(SCHEMA_KEY).is_none(), "the loader hands back the doc without the key");
            }
            _ => panic!("expected Ready"),
        }
    }
}

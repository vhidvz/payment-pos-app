//! The install transaction (spec §3.2): stage → journal → commit, the forward-only
//! `recover` that finishes a commit a crash interrupted, GC, supersede and rollback.
//!
//! Invariants: the journal is the commit point; a generation is referenced (by the
//! journal, then `prev.json`) only after its manifest is on disk; the desktop entry —
//! the version key — is renamed last; everything unreferenced is collectable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Layout, Report};
use crate::fsutil::{atomic_copy, atomic_write, fsync_dir, sha256_bytes, sha256_file};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub target: PathBuf,
    pub staged: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub txid: String,
    pub from: String,
    pub to: String,
    pub prev_gen: Option<String>,
    pub entries: Vec<JournalEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestFile {
    pub target: PathBuf,
    pub name: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// `"legacy"`, `"unknown"` or a semver.
    pub version: String,
    pub files: Vec<ManifestFile>,
}

#[derive(Serialize, Deserialize)]
struct PrevPointer {
    gen: String,
}

/// Where a target's new content comes from.
#[derive(Debug, Clone)]
pub enum Source {
    File(PathBuf),
    Bytes(Vec<u8>),
}
pub use Source as ItemSource;

#[derive(Debug, Clone)]
pub struct Planned {
    pub target: PathBuf,
    pub source: Source,
    pub mode: u32,
}

/// Named crash points — spec §8's fault injection. Reaching one returns at once, with no
/// cleanup, exactly as a killed process would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    AfterStaged(usize),
    MidSnapshot,
    AfterManifest,
    AfterJournal,
    AfterPrevPointer,
    AfterRename(usize),
    AfterJournalDeleted,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub fault: Option<Fault>,
    /// The current set is not consistent: never snapshot it.
    pub skip_snapshot: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TxnError {
    Io(String),
    Injected(Fault),
    SourceDamaged(PathBuf),
}

impl std::fmt::Display for TxnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Injected(p) => write!(f, "stopped at {p:?}"),
            Self::SourceDamaged(p) => write!(f, "the previous version is damaged ({})", p.display()),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecoverError {
    Corrupt(PathBuf),
    Inconsistent(PathBuf),
    Io(String),
}

impl std::fmt::Display for RecoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt(p) => write!(f, "{} does not match its checksum", p.display()),
            Self::Inconsistent(p) => write!(f, "{} is neither the old nor the new file", p.display()),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recovered {
    Nothing,
    Completed { to: String },
}

fn io(e: std::io::Error) -> TxnError {
    TxnError::Io(e.to_string())
}
fn rio(e: std::io::Error) -> RecoverError {
    RecoverError::Io(e.to_string())
}
fn hit(opts: &Options, f: Fault) -> Result<(), TxnError> {
    if opts.fault == Some(f) {
        Err(TxnError::Injected(f))
    } else {
        Ok(())
    }
}
fn parent(p: &Path) -> &Path {
    p.parent().unwrap_or(Path::new("."))
}
fn staged_path(target: &Path, txid: &str) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(format!(".new-{txid}"));
    PathBuf::from(s)
}

#[cfg(unix)]
fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0o644)
}
#[cfg(not(unix))]
fn mode_of(_: &Path) -> u32 {
    0o644
}

/// One transaction at a time per user: the update screen opened twice, or a CLI install
/// beside it, queue here instead of interleaving.
pub struct InstallLock {
    _file: std::fs::File,
}

impl InstallLock {
    pub fn acquire(layout: &Layout) -> std::io::Result<Self> {
        std::fs::create_dir_all(&layout.state)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(layout.install_lock_path())?;
        file.lock()?;
        Ok(Self { _file: file })
    }
}

pub fn prev_gen(layout: &Layout) -> Option<String> {
    let raw = std::fs::read(layout.prev_pointer()).ok()?;
    serde_json::from_slice::<PrevPointer>(&raw).ok().map(|p| p.gen)
}

fn write_pointer(layout: &Layout, gen: &str) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&PrevPointer { gen: gen.into() }).expect("pointer");
    atomic_write(&layout.prev_pointer(), &bytes, None, 0o644)
}

fn read_manifest(dir: &Path) -> Option<Manifest> {
    serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).ok()?).ok()
}

pub fn prev_version(layout: &Layout) -> Option<String> {
    read_manifest(&layout.generations_dir().join(prev_gen(layout)?)).map(|m| m.version)
}

fn read_journal(layout: &Layout) -> Option<Journal> {
    serde_json::from_slice(&std::fs::read(layout.journal_path()).ok()?).ok()
}

/// What an AppImage install puts down, in commit order — the desktop entry last.
pub fn planned_from_appimage(layout: &Layout, source: &super::Source, version: &str) -> Vec<Planned> {
    let mut out = vec![Planned {
        target: layout.app_path(),
        source: Source::File(source.appimage.clone()),
        mode: 0o755,
    }];
    let posd = source.appdir.join("usr/bin/posd");
    if posd.exists() {
        out.push(Planned { target: layout.posd_path(), source: Source::File(posd), mode: 0o755 });
    }
    for size in super::ICON_SIZES {
        let from = source
            .appdir
            .join(format!("usr/share/icons/hicolor/{size}x{size}/apps/payment-pos-app.png"));
        if from.exists() {
            out.push(Planned { target: layout.icon_path(size), source: Source::File(from), mode: 0o644 });
        }
    }
    out.push(Planned {
        target: layout.desktop_path(),
        source: Source::Bytes(super::desktop_entry(&layout.app_path(), version).into_bytes()),
        mode: 0o644,
    });
    out
}

/// Delete staged files and generations nothing references (spec §3.2 GC).
pub fn gc(layout: &Layout) {
    let journal = read_journal(layout);
    let keep: Vec<String> = prev_gen(layout)
        .into_iter()
        .chain(journal.as_ref().and_then(|j| j.prev_gen.clone()))
        .collect();
    let live: Vec<PathBuf> = journal.map(|j| j.entries.into_iter().map(|e| e.staged).collect()).unwrap_or_default();
    let targets = layout.targets();
    let prefixes: Vec<String> = targets
        .iter()
        .filter_map(|t| t.file_name().map(|n| format!("{}.new-", n.to_string_lossy())))
        .collect();
    let mut dirs: Vec<PathBuf> = targets.iter().map(|t| parent(t).to_path_buf()).collect();
    dirs.sort();
    dirs.dedup();
    for dir in dirs {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if prefixes.iter().any(|p| name.starts_with(p)) && !live.contains(&entry.path()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    for entry in std::fs::read_dir(layout.generations_dir()).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !keep.contains(&name) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn stage(
    layout: &Layout,
    planned: &[Planned],
    txid: &str,
    gen: &str,
    from: &str,
    opts: &Options,
    entries: &mut Vec<JournalEntry>,
) -> Result<Option<String>, TxnError> {
    for (i, p) in planned.iter().enumerate() {
        let staged = staged_path(&p.target, txid);
        let sha = match &p.source {
            Source::File(f) => {
                atomic_copy(f, &staged, p.mode).map_err(|e| TxnError::Io(format!("{}: {e}", f.display())))?
            }
            Source::Bytes(b) => {
                atomic_write(&staged, b, None, p.mode).map_err(io)?;
                sha256_bytes(b)
            }
        };
        entries.push(JournalEntry { target: p.target.clone(), staged, sha256: sha });
        hit(opts, Fault::AfterStaged(i))?;
    }
    if opts.skip_snapshot {
        return Ok(None);
    }
    let current: Vec<PathBuf> = layout.targets().into_iter().filter(|t| t.exists()).collect();
    if current.is_empty() {
        return Ok(None);
    }
    let dir = layout.generations_dir().join(gen);
    let mut files = Vec::new();
    for (i, t) in current.iter().enumerate() {
        let name = format!("{i}-{}", t.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let sha = atomic_copy(t, &dir.join(&name), mode_of(t)).map_err(io)?;
        files.push(ManifestFile { target: t.clone(), name, sha256: sha });
        if i == 0 {
            hit(opts, Fault::MidSnapshot)?;
        }
    }
    let manifest = Manifest { version: from.into(), files };
    atomic_write(&dir.join("manifest.json"), &serde_json::to_vec_pretty(&manifest).expect("manifest"), None, 0o644)
        .map_err(io)?;
    hit(opts, Fault::AfterManifest)?;
    Ok(Some(gen.to_string()))
}

fn commit(layout: &Layout, j: &Journal, opts: &Options) -> Result<Report, TxnError> {
    if let Some(gen) = &j.prev_gen {
        write_pointer(layout, gen).map_err(io)?;
        hit(opts, Fault::AfterPrevPointer)?;
    }
    let mut written = Vec::new();
    for (i, e) in j.entries.iter().enumerate() {
        std::fs::rename(&e.staged, &e.target).map_err(io)?;
        fsync_dir(parent(&e.target)).map_err(io)?;
        written.push(e.target.clone());
        hit(opts, Fault::AfterRename(i))?;
    }
    std::fs::remove_file(layout.journal_path()).map_err(io)?;
    fsync_dir(&layout.state).map_err(io)?;
    hit(opts, Fault::AfterJournalDeleted)?;
    gc(layout);
    Ok(Report { written, removed: Vec::new() })
}

fn run_locked(layout: &Layout, planned: &[Planned], to: &str, opts: &Options) -> Result<Report, TxnError> {
    debug_assert_eq!(planned.last().map(|p| &p.target), Some(&layout.desktop_path()), "desktop entry last");
    let from = super::installed_label(&super::installed(layout));
    let txid = uuid::Uuid::new_v4().simple().to_string();
    let gen = format!("gen-{txid}");
    std::fs::create_dir_all(&layout.state).map_err(io)?;
    let mut entries = Vec::new();
    let prev_gen = match stage(layout, planned, &txid, &gen, &from, opts, &mut entries) {
        Ok(g) => g,
        Err(TxnError::Injected(f)) => return Err(TxnError::Injected(f)), // a crash cleans nothing
        Err(e) => {
            // A real error (disk full, unreadable source): put everything back as it was.
            for entry in &entries {
                let _ = std::fs::remove_file(&entry.staged);
            }
            let _ = std::fs::remove_dir_all(layout.generations_dir().join(&gen));
            gc(layout);
            return Err(e);
        }
    };
    let journal = Journal { txid, from, to: to.into(), prev_gen, entries };
    atomic_write(&layout.journal_path(), &serde_json::to_vec_pretty(&journal).expect("journal"), None, 0o644)
        .map_err(io)?;
    hit(opts, Fault::AfterJournal)?;
    commit(layout, &journal, opts)
}

/// Forward-only and never destructive: verify every entry first, then finish the renames.
fn recover_locked(layout: &Layout) -> Result<Recovered, RecoverError> {
    let journal: Journal = match std::fs::read(layout.journal_path()) {
        Ok(raw) => serde_json::from_slice(&raw)
            .map_err(|e| RecoverError::Io(format!("the install journal is unreadable ({e})")))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            gc(layout);
            return Ok(Recovered::Nothing);
        }
        Err(e) => return Err(rio(e)),
    };
    for e in &journal.entries {
        if e.staged.exists() {
            if sha256_file(&e.staged).map_err(rio)? != e.sha256 {
                return Err(RecoverError::Corrupt(e.staged.clone()));
            }
        } else if !(e.target.exists() && sha256_file(&e.target).map_err(rio)? == e.sha256) {
            return Err(RecoverError::Inconsistent(e.target.clone()));
        }
    }
    if let Some(gen) = &journal.prev_gen {
        if layout.generations_dir().join(gen).join("manifest.json").exists() {
            write_pointer(layout, gen).map_err(rio)?;
        }
    }
    for e in &journal.entries {
        if e.staged.exists() {
            std::fs::rename(&e.staged, &e.target).map_err(rio)?;
            fsync_dir(parent(&e.target)).map_err(rio)?;
        }
    }
    std::fs::remove_file(layout.journal_path()).map_err(rio)?;
    fsync_dir(&layout.state).map_err(rio)?;
    gc(layout);
    Ok(Recovered::Completed { to: journal.to })
}

/// Finish whatever commit a crash interrupted; run before every boot decision.
pub fn recover(layout: &Layout) -> Result<Recovered, RecoverError> {
    // No state directory: nothing was ever installed this way (a package install, Windows),
    // so there is nothing to recover — and taking the lock would create one.
    if !layout.state.exists() {
        return Ok(Recovered::Nothing);
    }
    let _lock = InstallLock::acquire(layout).map_err(rio)?;
    recover_locked(layout)
}

fn supersede_locked(layout: &Layout) -> std::io::Result<()> {
    let path = layout.journal_path();
    if !path.exists() {
        return Ok(());
    }
    let txid = read_journal(layout).map(|j| j.txid).unwrap_or_else(|| "unreadable".into());
    std::fs::rename(&path, layout.state.join(format!("install-journal.failed-{txid}.json")))?;
    fsync_dir(&layout.state)
}

/// Every explicit install (`--install`, `--update`, `--rollback`, the update screen):
/// recover, or supersede what recovery could not reconcile — never replay it — then run a
/// fresh transaction over every target.
pub fn explicit(layout: &Layout, planned: &[Planned], to: &str, opts: &Options) -> Result<Report, TxnError> {
    let _lock = InstallLock::acquire(layout).map_err(io)?;
    let mut opts = opts.clone();
    if let Err(e) = recover_locked(layout) {
        tracing::warn!("an earlier install could not be finished ({e}); replacing the whole set");
        supersede_locked(layout).map_err(io)?;
        opts.skip_snapshot = true;
    }
    run_locked(layout, planned, to, &opts)
}

/// Bring back the previous generation — binaries, icons and the version key together.
pub fn rollback(layout: &Layout, opts: &Options) -> Result<(Report, String), TxnError> {
    let gen = prev_gen(layout).ok_or_else(|| TxnError::Io("there is no previous version to roll back to".into()))?;
    let dir = layout.generations_dir().join(&gen);
    let manifest = read_manifest(&dir).ok_or_else(|| TxnError::SourceDamaged(dir.join("manifest.json")))?;
    for f in &manifest.files {
        let p = dir.join(&f.name);
        if sha256_file(&p).ok().as_deref() != Some(f.sha256.as_str()) {
            return Err(TxnError::SourceDamaged(p));
        }
    }
    let planned: Vec<Planned> = manifest
        .files
        .iter()
        .map(|f| Planned {
            target: f.target.clone(),
            source: Source::File(dir.join(&f.name)),
            mode: mode_of(&dir.join(&f.name)),
        })
        .collect();
    let report = explicit(layout, &planned, &manifest.version, opts)?;
    Ok((report, manifest.version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::testing::{appimage, assert_clean, assert_set, layout, root};

    /// Install `version` from its fixture AppImage, optionally crashing at `fault`.
    fn put(l: &Layout, r: &Path, version: &str, fault: Option<Fault>) -> Result<Report, TxnError> {
        let src = appimage(r, version);
        explicit(l, &planned_from_appimage(l, &src, version), version, &Options { fault, skip_snapshot: false })
    }

    fn stage_faults(n: usize) -> Vec<Fault> {
        let mut f: Vec<Fault> = (0..n).map(Fault::AfterStaged).collect();
        f.extend([Fault::MidSnapshot, Fault::AfterManifest]);
        f
    }
    fn commit_faults(n: usize) -> Vec<Fault> {
        let mut f = vec![Fault::AfterJournal, Fault::AfterPrevPointer];
        f.extend((0..n).map(Fault::AfterRename));
        f.push(Fault::AfterJournalDeleted);
        f
    }

    /// Every start runs `recover`; on a machine that never installed the AppImage (a
    /// package install, Windows) it must leave no installer state behind.
    #[test]
    fn recovery_where_nothing_was_ever_installed_creates_nothing() {
        let r = root();
        let l = layout(&r);
        assert_eq!(recover(&l).unwrap(), Recovered::Nothing);
        assert!(!l.state.exists(), "{} was created", l.state.display());
    }

    #[test]
    fn a_fresh_install_leaves_no_generation_and_no_journal() {
        let r = root();
        let l = layout(&r);
        put(&l, &r, "1.0.0", None).unwrap();
        assert_set(&l, "1.0.0");
        assert_eq!(prev_gen(&l), None);
        assert_clean(&l);
    }

    #[test]
    fn an_update_keeps_the_previous_set_as_a_checksummed_generation() {
        let r = root();
        let l = layout(&r);
        put(&l, &r, "1.0.0", None).unwrap();
        put(&l, &r, "1.1.0", None).unwrap();
        assert_set(&l, "1.1.0");
        assert_eq!(prev_version(&l).as_deref(), Some("1.0.0"));
        assert_clean(&l);
    }

    /// Spec §8: a crash before the journal leaves the old set complete and every leftover
    /// collectable.
    #[test]
    fn a_crash_before_the_journal_changes_nothing() {
        let n = planned_from_appimage(&layout(&root()), &appimage(&root(), "x"), "x").len();
        for fault in stage_faults(n) {
            let r = root();
            let l = layout(&r);
            put(&l, &r, "1.0.0", None).unwrap();
            put(&l, &r, "1.1.0", None).unwrap(); // prev → 1.0.0
            assert_eq!(put(&l, &r, "1.2.0", Some(fault)), Err(TxnError::Injected(fault)));
            recover(&l).unwrap();
            assert_set(&l, "1.1.0");
            assert_eq!(prev_version(&l).as_deref(), Some("1.0.0"), "{fault:?}: prev untouched");
            assert_clean(&l);
        }
    }

    /// Spec §8: a crash anywhere after the journal is finished forward by `recover`.
    #[test]
    fn a_crash_after_the_journal_is_finished_forward() {
        let n = planned_from_appimage(&layout(&root()), &appimage(&root(), "x"), "x").len();
        for fault in commit_faults(n) {
            let r = root();
            let l = layout(&r);
            put(&l, &r, "1.0.0", None).unwrap();
            put(&l, &r, "1.1.0", None).unwrap();
            assert_eq!(put(&l, &r, "1.2.0", Some(fault)), Err(TxnError::Injected(fault)));
            let recovered = recover(&l).unwrap();
            if fault == Fault::AfterJournalDeleted {
                // The commit had finished; only GC was left.
                assert_eq!(recovered, Recovered::Nothing);
            } else {
                assert_eq!(recovered, Recovered::Completed { to: "1.2.0".into() }, "{fault:?}");
            }
            assert_set(&l, "1.2.0");
            assert_eq!(prev_version(&l).as_deref(), Some("1.1.0"), "{fault:?}");
            assert_clean(&l);
            assert_eq!(recover(&l).unwrap(), Recovered::Nothing, "idempotent");
        }
    }

    #[test]
    fn staging_io_error_cleans_up() {
        let r = root();
        let l = layout(&r);
        put(&l, &r, "1.0.0", None).unwrap();
        let src = appimage(&r, "1.1.0");
        let mut planned = planned_from_appimage(&l, &src, "1.1.0");
        planned[2].source = ItemSource::File(r.join("does-not-exist"));
        assert!(matches!(explicit(&l, &planned, "1.1.0", &Options::default()), Err(TxnError::Io(_))));
        assert_set(&l, "1.0.0");
        assert_clean(&l);
    }

    #[test]
    fn a_corrupt_staged_file_stops_recovery_and_an_explicit_install_supersedes_it() {
        let r = root();
        let l = layout(&r);
        put(&l, &r, "1.0.0", None).unwrap();
        put(&l, &r, "1.1.0", Some(Fault::AfterJournal)).unwrap_err();
        let j = read_journal(&l).unwrap();
        std::fs::write(&j.entries[0].staged, b"garbage").unwrap();

        assert!(matches!(recover(&l), Err(RecoverError::Corrupt(_))));
        assert_eq!(
            crate::install::installed(&l),
            crate::install::Installed::Unknown(crate::install::UnknownReason::UnreconciledJournal)
        );
        let prev_before = prev_gen(&l);

        put(&l, &r, "1.1.0", None).unwrap();
        assert_set(&l, "1.1.0");
        assert_eq!(prev_gen(&l), prev_before, "a mixed set never becomes the rollback target");
        let failed: Vec<_> = std::fs::read_dir(&l.state)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("install-journal.failed-"))
            .collect();
        assert_eq!(failed.len(), 1, "kept for diagnosis");
    }

    #[test]
    fn a_target_neither_staged_nor_matching_is_inconsistent() {
        let r = root();
        let l = layout(&r);
        put(&l, &r, "1.0.0", None).unwrap();
        put(&l, &r, "1.1.0", Some(Fault::AfterRename(0))).unwrap_err();
        std::fs::write(l.app_path(), b"tampered").unwrap();
        assert!(matches!(recover(&l), Err(RecoverError::Inconsistent(_))));
        assert_eq!(std::fs::read(l.app_path()).unwrap(), b"tampered", "recovery touched nothing");
    }

    #[test]
    fn rollback_brings_back_the_matching_version_key_and_toggles() {
        let r = root();
        let l = layout(&r);
        put(&l, &r, "1.0.0", None).unwrap();
        put(&l, &r, "1.1.0", None).unwrap();
        let (_, v) = rollback(&l, &Options::default()).unwrap();
        assert_eq!(v, "1.0.0");
        assert_set(&l, "1.0.0");
        assert_eq!(
            crate::install::installed(&l),
            crate::install::Installed::Version(semver::Version::new(1, 0, 0))
        );
        let (_, v) = rollback(&l, &Options::default()).unwrap();
        assert_eq!(v, "1.1.0");
        assert_set(&l, "1.1.0");
        assert_clean(&l);
    }

    #[test]
    fn a_damaged_previous_generation_is_refused() {
        let r = root();
        let l = layout(&r);
        put(&l, &r, "1.0.0", None).unwrap();
        put(&l, &r, "1.1.0", None).unwrap();
        let gen = l.generations_dir().join(prev_gen(&l).unwrap());
        let victim = std::fs::read_dir(&gen)
            .unwrap()
            .flatten()
            .find(|e| e.file_name() != "manifest.json")
            .unwrap()
            .path();
        std::fs::write(&victim, b"bitrot").unwrap();
        assert!(matches!(rollback(&l, &Options::default()), Err(TxnError::SourceDamaged(_))));
        assert_set(&l, "1.1.0");
    }

    #[test]
    fn concurrent_explicit_installs_serialize() {
        let r = root();
        let l = layout(&r);
        put(&l, &r, "1.0.0", None).unwrap();
        let threads: Vec<_> = ["1.1.0", "1.2.0"]
            .into_iter()
            .map(|v| {
                let (r, l) = (r.clone(), l.clone());
                std::thread::spawn(move || put(&l, &r, v, None).unwrap())
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let now = std::fs::read_to_string(l.app_path()).unwrap();
        let v = now.trim_start_matches("#appimage ");
        assert_set(&l, v);
        assert_clean(&l);
    }

    #[test]
    fn uninstall_takes_the_generations_and_the_pointer_too() {
        let r = root();
        let l = layout(&r);
        put(&l, &r, "1.0.0", None).unwrap();
        put(&l, &r, "1.1.0", None).unwrap();
        crate::install::uninstall(&l).unwrap();
        assert!(!l.generations_dir().exists());
        assert!(!l.prev_pointer().exists());
    }
}

//! Desktop integration for the AppImage build.
//!
//! An AppImage is a single portable file: nothing puts it on the menu, gives it
//! an icon, or makes `posd` reachable. Doing that by hand is a page of `install`
//! commands, so the app does it for itself.
//!
//! Everything lands under the user's own XDG directories, so no privilege is
//! needed and removal is exact. Nothing here is distribution-specific: the
//! desktop entry and icon locations are the same on Arch, Debian and Fedora
//! alike. Native packaging deliberately stays out of scope — an AppImage that
//! wrote into a package manager's territory would fight it.

pub mod txn;

use std::path::{Path, PathBuf};

/// Basename used for every file we install, so removal can be exact.
const STEM: &str = "ledger-pos";
const ICON_SIZES: [u32; 4] = [32, 128, 256, 512];

/// Where the per-user install writes.
#[derive(Debug, Clone)]
pub struct Layout {
    bin: PathBuf,
    applications: PathBuf,
    icons: PathBuf,
    /// The installer's own state: the rollback generation, its pointer, the journal.
    state: PathBuf,
}

impl Layout {
    /// The real per-user locations.
    pub fn current() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let data = dirs::data_dir().unwrap_or_else(|| home.join(".local/share"));
        Self {
            bin: home.join(".local/bin"),
            applications: data.join("applications"),
            icons: data.join("icons/hicolor"),
            state: data.join("ledger-pos"),
        }
    }

    /// Same shape under an arbitrary root — the seam the tests use.
    pub fn under(home: &Path) -> Self {
        Self {
            bin: home.join(".local/bin"),
            applications: home.join(".local/share/applications"),
            icons: home.join(".local/share/icons/hicolor"),
            state: home.join(".local/share/ledger-pos"),
        }
    }

    pub fn app_path(&self) -> PathBuf {
        self.bin.join(format!("{STEM}.AppImage"))
    }
    pub fn posd_path(&self) -> PathBuf {
        self.bin.join("posd")
    }
    pub fn desktop_path(&self) -> PathBuf {
        self.applications.join(format!("{STEM}.desktop"))
    }
    pub fn icon_path(&self, size: u32) -> PathBuf {
        self.icons.join(format!("{size}x{size}/apps/{STEM}.png"))
    }
    pub fn journal_path(&self) -> PathBuf {
        self.state.join("install-journal.json")
    }
    pub fn prev_pointer(&self) -> PathBuf {
        self.state.join("prev.json")
    }
    pub fn generations_dir(&self) -> PathBuf {
        self.state.join("generations")
    }
    pub fn install_lock_path(&self) -> PathBuf {
        self.state.join(".install.lock")
    }
    /// Every file an install owns, **the desktop entry last** — it carries the version
    /// key, so it is the last thing a commit changes (spec §3.2).
    pub fn targets(&self) -> Vec<PathBuf> {
        let mut t = vec![self.app_path(), self.posd_path()];
        t.extend(ICON_SIZES.iter().map(|s| self.icon_path(*s)));
        t.push(self.desktop_path());
        t
    }
}

/// What is being installed: the AppImage file itself, and the mounted AppDir it
/// is currently running from (where `posd` and the icons live).
#[derive(Debug, Clone)]
pub struct Source {
    pub appimage: PathBuf,
    pub appdir: PathBuf,
}

impl Source {
    /// Detect an AppImage run. `None` means the app was started some other way
    /// (a distribution package, or straight out of the build directory) and
    /// there is nothing to integrate.
    pub fn detect() -> Option<Self> {
        let appimage = PathBuf::from(std::env::var_os("APPIMAGE")?);
        let appdir = std::env::var_os("APPDIR").map(PathBuf::from)?;
        Some(Self { appimage, appdir })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Absolute paths that now exist because of this operation.
    pub written: Vec<PathBuf>,
    /// Absolute paths that were removed.
    pub removed: Vec<PathBuf>,
}

pub fn is_installed(layout: &Layout) -> bool {
    layout.app_path().exists() && layout.desktop_path().exists()
}

/// Desktop Entry `X-*` extension key carrying the installed version (spec §3.1).
pub const VERSION_KEY: &str = "X-LedgerPOS-Version";

pub fn desktop_entry(app: &Path, version: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name=Ledger POS\n\
         Comment=Payment POS terminals as a local REST API\n\
         Exec={exec}\n\
         Icon={STEM}\n\
         Terminal=false\n\
         Categories=Office;Finance;\n\
         StartupWMClass=payment-pos-app\n\
         {VERSION_KEY}={version}\n",
        exec = quote_exec(app),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownReason {
    /// The key is there but is not a version.
    BadVersionKey(String),
    /// An interrupted install that recovery could not finish.
    UnreconciledJournal,
}

impl std::fmt::Display for UnknownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadVersionKey(v) => write!(f, "the menu entry names version \"{v}\""),
            Self::UnreconciledJournal => write!(f, "an earlier install was interrupted and could not be finished"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    None,
    /// Installed without a version key: the known pre-1.0 shape, older than every version.
    Legacy,
    Version(semver::Version),
    /// Never ordered against anything; replaced only on explicit confirmation.
    Unknown(UnknownReason),
}

pub fn installed(layout: &Layout) -> Installed {
    // `recover()` runs before anyone asks; a journal still here is one it could not finish.
    if layout.journal_path().exists() {
        return Installed::Unknown(UnknownReason::UnreconciledJournal);
    }
    if !is_installed(layout) {
        return Installed::None;
    }
    let entry = std::fs::read_to_string(layout.desktop_path()).unwrap_or_default();
    let prefix = format!("{VERSION_KEY}=");
    match entry.lines().find_map(|l| l.strip_prefix(prefix.as_str())) {
        None => Installed::Legacy,
        Some(raw) => match semver::Version::parse(raw.trim()) {
            Ok(v) => Installed::Version(v),
            Err(_) => Installed::Unknown(UnknownReason::BadVersionKey(raw.trim().to_string())),
        },
    }
}

/// How an install is named in a journal or a generation manifest.
pub fn installed_label(installed: &Installed) -> String {
    match installed {
        Installed::None => "none".into(),
        Installed::Legacy => "legacy".into(),
        Installed::Version(v) => v.to_string(),
        Installed::Unknown(_) => "unknown".into(),
    }
}

/// Desktop Entry spec: an `Exec` argument containing spaces must be quoted, and
/// a handful of characters stay special inside those quotes.
fn quote_exec(path: &Path) -> String {
    format!(
        "\"{}\"",
        path.display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$")
    )
}

/// Put the app on the menu, give it an icon, make `posd` reachable — transactionally:
/// an interruption is finished by the next start, never left as a mix (spec §3.2).
/// Re-running repairs rather than fails, so it doubles as a "fix my install".
pub fn install(layout: &Layout, source: &Source) -> Result<Report, String> {
    let planned = txn::planned_from_appimage(layout, source, crate::version::VERSION);
    let report = txn::explicit(layout, &planned, crate::version::VERSION, &txn::Options::default())
        .map_err(|e| e.to_string())?;
    refresh_desktop_caches(layout);
    Ok(report)
}

/// Swap the previous generation back in. Returns the version now installed.
pub fn rollback(layout: &Layout) -> Result<String, String> {
    let (_, version) = txn::rollback(layout, &txn::Options::default()).map_err(|e| e.to_string())?;
    refresh_desktop_caches(layout);
    Ok(version)
}

/// Take back exactly what `install` put down, and nothing else — settings and
/// the administrator file are not ours to delete.
pub fn uninstall(layout: &Layout) -> Result<Report, String> {
    let mut removed = Vec::new();
    let mut candidates = vec![layout.app_path(), layout.posd_path(), layout.desktop_path()];
    candidates.extend(ICON_SIZES.iter().map(|s| layout.icon_path(*s)));

    for path in candidates {
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot remove {}: {e}", path.display())),
        }
    }
    // The rollback generation, its pointer and any journal are ours too.
    let _ = std::fs::remove_dir_all(layout.generations_dir());
    for p in [layout.prev_pointer(), layout.journal_path(), layout.install_lock_path()] {
        let _ = std::fs::remove_file(p);
    }
    for entry in std::fs::read_dir(&layout.state).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with("install-journal.failed-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    txn::gc(layout); // staged leftovers
    let _ = std::fs::remove_dir(&layout.state); // only when empty
    refresh_desktop_caches(layout);
    Ok(Report { written: Vec::new(), removed })
}

/// Best effort: some desktops notice a new entry immediately, others need a nudge.
/// A missing tool is not a failure — the entry is on disk either way.
fn refresh_desktop_caches(layout: &Layout) {
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&layout.applications)
        .output();
    let _ = std::process::Command::new("gtk-update-icon-cache")
        .args(["-f", "-t"])
        .arg(&layout.icons)
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for a mounted AppImage: the file itself plus the AppDir that
    /// carries `posd` and the icons.
    fn fake_appimage() -> (PathBuf, Source, Layout) {
        let root = std::env::temp_dir().join(format!("ledger-pos-inst-{}", uuid::Uuid::new_v4()));
        let appdir = root.join("mnt");
        std::fs::create_dir_all(appdir.join("usr/bin")).unwrap();
        std::fs::write(appdir.join("usr/bin/posd"), b"#posd binary").unwrap();
        for size in [128u32, 256] {
            let p = appdir.join(format!("usr/share/icons/hicolor/{size}x{size}/apps/payment-pos-app.png"));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"#png").unwrap();
        }
        let appimage = root.join("Downloads/Ledger POS_0.1.0_amd64.AppImage");
        std::fs::create_dir_all(appimage.parent().unwrap()).unwrap();
        std::fs::write(&appimage, b"#appimage payload").unwrap();

        let home = root.join("home");
        (home.clone(), Source { appimage, appdir }, Layout::under(&home))
    }

    #[test]
    fn nothing_is_installed_to_begin_with() {
        let (_, _, layout) = fake_appimage();
        assert!(!is_installed(&layout));
    }

    #[test]
    fn install_puts_the_app_the_launcher_and_an_icon_in_place() {
        let (_, source, layout) = fake_appimage();

        install(&layout, &source).expect("install");

        assert!(layout.app_path().exists(), "the AppImage should be copied in");
        assert!(layout.desktop_path().exists(), "a desktop entry should be written");
        assert!(layout.icon_path(128).exists(), "an icon should be installed");
        assert!(is_installed(&layout));
    }

    /// The whole point: the menu entry must launch the *installed* copy, so the
    /// original can be deleted from wherever it was downloaded.
    #[test]
    fn the_desktop_entry_launches_the_installed_copy() {
        let (_, source, layout) = fake_appimage();
        install(&layout, &source).unwrap();

        let entry = std::fs::read_to_string(layout.desktop_path()).unwrap();
        let exec = entry
            .lines()
            .find_map(|l| l.strip_prefix("Exec="))
            .expect("an Exec line");

        assert!(
            exec.contains(layout.app_path().to_str().unwrap()),
            "Exec should point at the installed copy, got: {exec}"
        );
        assert!(
            !exec.contains("Downloads"),
            "Exec must not point back at the download, got: {exec}"
        );
    }

    /// A path with a space needs quoting or desktop environments refuse the entry.
    #[test]
    fn the_desktop_entry_is_well_formed() {
        let (_, source, layout) = fake_appimage();
        install(&layout, &source).unwrap();
        let entry = std::fs::read_to_string(layout.desktop_path()).unwrap();

        assert!(entry.starts_with("[Desktop Entry]"), "{entry}");
        for key in ["Type=Application", "Name=Ledger POS", "Terminal=false"] {
            assert!(entry.contains(key), "missing {key} in:\n{entry}");
        }
        assert!(entry.contains(&format!("Icon={STEM}")), "icon should be referenced by name:\n{entry}");
    }

    /// `posd admin …` is in the README; it has to be runnable without digging
    /// inside the AppImage.
    #[test]
    fn install_exposes_posd_as_an_executable() {
        let (_, source, layout) = fake_appimage();
        install(&layout, &source).unwrap();

        let posd = layout.posd_path();
        assert!(posd.exists(), "posd should be installed next to the app");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&posd).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "posd must be executable, mode {mode:o}");
        }
    }

    #[test]
    fn the_installed_app_is_executable() {
        let (_, source, layout) = fake_appimage();
        install(&layout, &source).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(layout.app_path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "mode {mode:o}");
        }
    }

    /// Re-running it must repair, not fail.
    #[test]
    fn installing_twice_is_not_an_error() {
        let (_, source, layout) = fake_appimage();
        install(&layout, &source).unwrap();
        install(&layout, &source).expect("second install should succeed");
        assert!(is_installed(&layout));
    }

    #[test]
    fn uninstall_removes_exactly_what_install_wrote() {
        let (_, source, layout) = fake_appimage();
        let put = install(&layout, &source).unwrap();

        let taken = uninstall(&layout).expect("uninstall");

        for p in &put.written {
            assert!(!p.exists(), "{} should have been removed", p.display());
        }
        assert_eq!(
            taken.removed.len(),
            put.written.len(),
            "removed {:?} vs written {:?}",
            taken.removed,
            put.written
        );
        assert!(!is_installed(&layout));
    }

    /// Removing the app must not remove the terminal configuration.
    #[test]
    fn uninstall_leaves_the_users_settings_alone() {
        let (home, source, layout) = fake_appimage();
        let settings = home.join(".config/ledger-pos/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, b"{}").unwrap();

        install(&layout, &source).unwrap();
        uninstall(&layout).unwrap();

        assert!(settings.exists(), "settings must survive an uninstall");
    }

    #[test]
    fn uninstalling_when_nothing_is_installed_is_not_an_error() {
        let (_, _, layout) = fake_appimage();
        assert_eq!(uninstall(&layout).unwrap().removed, Vec::<PathBuf>::new());
    }

    fn entry_with(layout: &Layout, key_line: &str) {
        std::fs::create_dir_all(layout.app_path().parent().unwrap()).unwrap();
        std::fs::write(layout.app_path(), b"#app").unwrap();
        std::fs::create_dir_all(layout.desktop_path().parent().unwrap()).unwrap();
        std::fs::write(layout.desktop_path(), format!("[Desktop Entry]\nName=Ledger POS\n{key_line}")).unwrap();
    }

    #[test]
    fn install_writes_its_version_into_the_desktop_entry() {
        let (_, source, layout) = fake_appimage();
        install(&layout, &source).unwrap();
        let entry = std::fs::read_to_string(layout.desktop_path()).unwrap();
        assert!(entry.contains(&format!("{VERSION_KEY}={}\n", crate::version::VERSION)), "{entry}");
        assert_eq!(installed(&layout), Installed::Version(crate::version::current()));
    }

    #[test]
    fn classification_of_installed_copies() {
        let (_, _, layout) = fake_appimage();
        assert_eq!(installed(&layout), Installed::None);
        entry_with(&layout, "");
        assert_eq!(installed(&layout), Installed::Legacy, "no key: the known pre-1.0 shape");
        entry_with(&layout, "X-LedgerPOS-Version=banana\n");
        assert_eq!(installed(&layout), Installed::Unknown(UnknownReason::BadVersionKey("banana".into())));
        entry_with(&layout, "X-LedgerPOS-Version=1.2.3\n");
        assert_eq!(installed(&layout), Installed::Version(semver::Version::new(1, 2, 3)));
    }

    #[test]
    fn version_key_tolerates_whitespace() {
        let (_, _, layout) = fake_appimage();
        entry_with(&layout, "X-LedgerPOS-Version= 1.0.0 \r\n");
        assert_eq!(installed(&layout), Installed::Version(semver::Version::new(1, 0, 0)));
    }

    #[test]
    fn a_journal_left_behind_makes_the_install_unknown() {
        let (_, _, layout) = fake_appimage();
        entry_with(&layout, "X-LedgerPOS-Version=1.0.0\n");
        std::fs::create_dir_all(layout.journal_path().parent().unwrap()).unwrap();
        std::fs::write(layout.journal_path(), b"{}").unwrap();
        assert_eq!(installed(&layout), Installed::Unknown(UnknownReason::UnreconciledJournal));
    }

    #[test]
    fn the_desktop_entry_is_the_last_target() {
        let (_, _, layout) = fake_appimage();
        assert_eq!(layout.targets().last(), Some(&layout.desktop_path()));
    }
}

/// Fixtures shared by the transaction and engine tests: an AppImage whose every file
/// carries its version, so an old set and a new one differ byte for byte.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub fn root() -> PathBuf {
        let r = std::env::temp_dir().join(format!("ledger-txn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&r).unwrap();
        r
    }

    pub fn layout(root: &Path) -> Layout {
        Layout::under(&root.join("home"))
    }

    pub fn appimage(root: &Path, version: &str) -> Source {
        let base = root.join(format!("src-{version}"));
        let appdir = base.join("mnt");
        std::fs::create_dir_all(appdir.join("usr/bin")).unwrap();
        std::fs::write(appdir.join("usr/bin/posd"), format!("#posd {version}")).unwrap();
        for size in [128u32, 256] {
            let p = appdir.join(format!("usr/share/icons/hicolor/{size}x{size}/apps/payment-pos-app.png"));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, format!("#png {size} {version}")).unwrap();
        }
        let file = base.join(format!("Ledger POS_{version}_amd64.AppImage"));
        std::fs::write(&file, format!("#appimage {version}")).unwrap();
        Source { appimage: file, appdir }
    }

    /// Every target holds exactly `version`'s bytes, and the menu entry names it.
    pub fn assert_set(layout: &Layout, version: &str) {
        let read = |p: PathBuf| std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert_eq!(read(layout.app_path()), format!("#appimage {version}"));
        assert_eq!(read(layout.posd_path()), format!("#posd {version}"));
        assert_eq!(read(layout.icon_path(128)), format!("#png 128 {version}"));
        assert!(read(layout.desktop_path()).contains(&format!("{VERSION_KEY}={version}\n")));
    }

    /// No staged file and no unreferenced generation is left behind.
    pub fn assert_clean(layout: &Layout) {
        for t in layout.targets() {
            let dir = t.parent().unwrap();
            let prefix = format!("{}.new-", t.file_name().unwrap().to_string_lossy());
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                assert!(!e.file_name().to_string_lossy().starts_with(&prefix), "leftover {}", e.path().display());
            }
        }
        let gens: Vec<_> = std::fs::read_dir(layout.generations_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let kept: Vec<String> = super::txn::prev_gen(layout).into_iter().collect();
        assert_eq!(gens, kept, "only the generation prev.json names survives");
        assert!(!layout.journal_path().exists());
    }
}

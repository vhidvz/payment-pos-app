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
        }
    }

    /// Same shape under an arbitrary root — the seam the tests use.
    pub fn under(home: &Path) -> Self {
        Self {
            bin: home.join(".local/bin"),
            applications: home.join(".local/share/applications"),
            icons: home.join(".local/share/icons/hicolor"),
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

fn copy_executable(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    // Remove first: overwriting a file that is currently executing fails with ETXTBSY.
    let _ = std::fs::remove_file(to);
    std::fs::copy(from, to)
        .map_err(|e| format!("cannot copy to {}: {e}", to.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot make {} executable: {e}", to.display()))?;
    }
    Ok(())
}

fn write_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, contents).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Put the app on the menu, give it an icon, and make `posd` reachable.
/// Re-running repairs rather than fails, so it doubles as a "fix my install".
pub fn install(layout: &Layout, source: &Source) -> Result<Report, String> {
    let mut written = Vec::new();

    let app = layout.app_path();
    copy_executable(&source.appimage, &app)?;
    written.push(app.clone());

    // `posd` rides inside the AppImage; lift it out so the administrator
    // commands in the README work without digging into the bundle.
    let posd_src = source.appdir.join("usr/bin/posd");
    if posd_src.exists() {
        let posd = layout.posd_path();
        copy_executable(&posd_src, &posd)?;
        written.push(posd);
    }

    for size in ICON_SIZES {
        let from = source
            .appdir
            .join(format!("usr/share/icons/hicolor/{size}x{size}/apps/payment-pos-app.png"));
        if !from.exists() {
            continue;
        }
        let to = layout.icon_path(size);
        let bytes = std::fs::read(&from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
        write_file(&to, &bytes)?;
        written.push(to);
    }

    let desktop = layout.desktop_path();
    write_file(
        &desktop,
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
             StartupWMClass=payment-pos-app\n",
            exec = quote_exec(&app),
        )
        .as_bytes(),
    )?;
    written.push(desktop);

    refresh_desktop_caches(layout);
    Ok(Report { written, removed: Vec::new() })
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
}

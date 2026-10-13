//! Whether this run is a normal start or an update screen (spec §3.1).

use std::path::Path;

use crate::install::{Installed, UnknownReason};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Boot {
    Normal,
    /// `from: None` — a pre-1.0 install, whose version nobody recorded.
    UpdateOffer { from: Option<semver::Version> },
    ReplaceUnknown { reason: UnknownReason },
    NewerInstalled { installed: semver::Version },
}

/// The boot decision, made before Tauri starts — before the single-instance plugin could
/// hand this launch to an older copy running in the tray.
pub fn plan(
    current: &semver::Version,
    installed: &Installed,
    is_appimage: bool,
    running_from_installed: bool,
) -> Boot {
    if !is_appimage || running_from_installed {
        return Boot::Normal;
    }
    match installed {
        Installed::None => Boot::Normal,
        Installed::Legacy => Boot::UpdateOffer { from: None },
        Installed::Unknown(reason) => Boot::ReplaceUnknown { reason: reason.clone() },
        Installed::Version(v) if v == current => Boot::Normal,
        Installed::Version(v) if v < current => Boot::UpdateOffer { from: Some(v.clone()) },
        Installed::Version(v) => Boot::NewerInstalled { installed: v.clone() },
    }
}

/// Whether two paths name one file — through symlinks and relative paths. A missing path
/// is never the same file.
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    #[test]
    fn the_boot_table() {
        let cur = v("1.1.0");
        let at = |i: &Installed| plan(&cur, i, true, false);
        assert_eq!(plan(&cur, &Installed::Legacy, false, false), Boot::Normal, "not an AppImage");
        assert_eq!(plan(&cur, &Installed::Legacy, true, true), Boot::Normal, "the installed copy itself");
        assert_eq!(at(&Installed::None), Boot::Normal);
        assert_eq!(at(&Installed::Version(v("1.1.0"))), Boot::Normal);
        assert_eq!(at(&Installed::Version(v("1.0.0"))), Boot::UpdateOffer { from: Some(v("1.0.0")) });
        assert_eq!(at(&Installed::Legacy), Boot::UpdateOffer { from: None });
        assert_eq!(at(&Installed::Version(v("2.0.0"))), Boot::NewerInstalled { installed: v("2.0.0") });
        let unknown = UnknownReason::BadVersionKey("x".into());
        assert_eq!(at(&Installed::Unknown(unknown.clone())), Boot::ReplaceUnknown { reason: unknown });
    }

    #[test]
    fn prerelease_orders_below_its_release() {
        assert_eq!(
            plan(&v("1.0.1"), &Installed::Version(v("1.0.1-test")), true, false),
            Boot::UpdateOffer { from: Some(v("1.0.1-test")) }
        );
        assert_eq!(
            plan(&v("1.0.1-test"), &Installed::Version(v("1.0.1")), true, false),
            Boot::NewerInstalled { installed: v("1.0.1") }
        );
    }

    #[cfg(unix)]
    #[test]
    fn same_file_sees_through_symlinks() {
        let d = std::env::temp_dir().join(format!("ledger-same-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        let real = d.join("ledger-pos.AppImage");
        std::fs::write(&real, b"x").unwrap();
        let link = d.join("link.AppImage");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(same_file(&link, &real));
        assert!(same_file(&d.join("./ledger-pos.AppImage"), &real));
        assert!(!same_file(&d.join("missing"), &real));
    }
}

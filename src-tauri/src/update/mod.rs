//! Whether this run is a normal start or an update screen (spec §3.1).

pub mod engine;
pub mod system;

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

/// What the frontend's `app.vue` asks first (`boot_mode`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootInfo {
    pub mode: &'static str,
    pub current: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A copy is running (it owns the single-instance name).
    pub running: bool,
}

impl BootInfo {
    pub fn normal() -> Self {
        Self::from_boot(&Boot::Normal, false)
    }

    pub fn from_boot(boot: &Boot, running: bool) -> Self {
        let base = |mode| Self {
            mode,
            current: crate::version::VERSION.to_string(),
            installed: None,
            reason: None,
            detail: None,
            running,
        };
        match boot {
            Boot::Normal => base("normal"),
            Boot::UpdateOffer { from } => {
                Self { installed: from.as_ref().map(|v| v.to_string()), ..base("updateOffer") }
            }
            Boot::ReplaceUnknown { reason } => Self {
                reason: Some(match reason {
                    UnknownReason::BadVersionKey(_) => "badVersionKey",
                    UnknownReason::UnreconciledJournal => "unreconciledJournal",
                    UnknownReason::UnreadableEntry(_) => "unreadableEntry",
                }),
                detail: Some(reason.to_string()),
                ..base("replaceUnknown")
            },
            Boot::NewerInstalled { installed } => {
                Self { installed: Some(installed.to_string()), ..base("newerInstalled") }
            }
        }
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

    #[test]
    fn boot_info_names_each_mode() {
        let info = |b: Boot| BootInfo::from_boot(&b, true);
        assert_eq!(info(Boot::Normal).mode, "normal");
        let offer = info(Boot::UpdateOffer { from: Some(v("1.0.0")) });
        assert_eq!((offer.mode, offer.installed.as_deref(), offer.running), ("updateOffer", Some("1.0.0"), true));
        assert_eq!(info(Boot::UpdateOffer { from: None }).installed, None, "pre-1.0");
        let unknown = info(Boot::ReplaceUnknown { reason: UnknownReason::UnreconciledJournal });
        assert_eq!((unknown.mode, unknown.reason), ("replaceUnknown", Some("unreconciledJournal")));
        let newer = info(Boot::NewerInstalled { installed: v("2.0.0") });
        assert_eq!((newer.mode, newer.installed.as_deref()), ("newerInstalled", Some("2.0.0")));
        assert_eq!(BootInfo::normal().current, crate::version::VERSION);
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

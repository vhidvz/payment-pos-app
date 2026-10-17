//! The machine the update engine acts on: D-Bus, `/proc`, process spawning — behind a
//! trait so every engine branch is tested with a script (spec §3.3).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The single-instance plugin's bus name for `identifier` `io.vhidvz.ledgerpos`
/// (tauri-plugin-single-instance 2.4.3, `platform_impl/linux.rs:37-49`).
pub const NAME: &str = "io.vhidvz.ledgerpos.SingleInstance";
pub const PATH: &str = "/io/vhidvz/ledgerpos/SingleInstance";
pub const INTERFACE: &str = "org.SingleInstance.DBus";
/// Passed through `ExecuteCallback`; a 1.0.0+ instance drains and exits on it.
pub const QUIT_FOR_UPDATE: &str = "--quit-for-update";

pub trait System: Send + Sync {
    /// Reconcile the background API onto the installed `posd` ([`crate::daemon`]):
    /// `Ok(what it found)`, or why it could not.
    fn ensure_daemon(&self) -> Result<String, String>;
    /// The process that owns the single-instance name, if any.
    fn instance_pid(&self) -> Option<u32>;
    fn request_quit(&self) -> Result<(), String>;
    fn spawn_detached(&self, exe: &Path) -> Result<u32, String>;
    /// `Some(reason)` once the spawned child exited unsuccessfully.
    fn child_failed(&self, pid: u32) -> Option<String>;
    /// The `APPIMAGE` a process was started from.
    fn appimage_of(&self, pid: u32) -> Option<PathBuf>;
    fn sleep(&self, d: Duration);
    fn now(&self) -> Instant;
}

/// `/proc/<pid>` entries owned by `uid` whose binary is named `posd` — a binary replaced
/// on disk reads `posd (deleted)` and still counts.
pub fn scan_posd(proc_root: &Path, uid: u32) -> Vec<u32> {
    scan_posd_exe(proc_root, uid).into_iter().map(|(pid, _)| pid).collect()
}

/// [`scan_posd`] with the binary each one runs, as `/proc/<pid>/exe` reads.
pub fn scan_posd_exe(proc_root: &Path, uid: u32) -> Vec<(u32, PathBuf)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(proc_root).into_iter().flatten().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
        let dir = entry.path();
        let owner = std::fs::read_to_string(dir.join("status")).ok().and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|l| l.split_whitespace().next())
                .and_then(|u| u.parse::<u32>().ok())
        });
        if owner != Some(uid) {
            continue;
        }
        let Ok(exe) = std::fs::read_link(dir.join("exe")) else { continue };
        let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name == "posd" || name == "posd (deleted)" {
            out.push((pid, exe));
        }
    }
    out.sort_unstable();
    out
}

/// The environment for a copy started from inside this AppImage (ruling R4): without our
/// AppImage's identity, and without anything pointing into our mount, which vanishes when
/// this process exits. `:`-lists keep their other elements.
pub fn clean_env(vars: impl IntoIterator<Item = (String, String)>, appdir: Option<&str>) -> Vec<(String, String)> {
    const IDENTITY: [&str; 4] = ["APPIMAGE", "APPDIR", "ARGV0", "OWD"];
    vars.into_iter()
        .filter(|(k, _)| !IDENTITY.contains(&k.as_str()))
        .filter_map(|(k, v)| {
            let Some(dir) = appdir.filter(|d| !d.is_empty()) else { return Some((k, v)) };
            if !v.contains(dir) {
                return Some((k, v));
            }
            if v.contains(':') {
                let kept: Vec<&str> = v.split(':').filter(|p| !p.starts_with(dir)).collect();
                (!kept.is_empty()).then(|| (k, kept.join(":")))
            } else {
                None
            }
        })
        .collect()
}

pub struct RealSystem {
    children: std::sync::Mutex<std::collections::HashMap<u32, std::process::Child>>,
}

impl RealSystem {
    pub fn new() -> Self {
        Self { children: Default::default() }
    }
}

impl Default for RealSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "linux")]
mod dbus {
    use super::{INTERFACE, NAME, PATH};
    use zbus::blocking::{fdo::DBusProxy, Connection};
    use zbus::names::BusName;

    pub fn owner_pid() -> Result<Option<u32>, String> {
        let conn = Connection::session().map_err(|e| e.to_string())?;
        let proxy = DBusProxy::new(&conn).map_err(|e| e.to_string())?;
        let name = BusName::try_from(NAME).map_err(|e| e.to_string())?;
        if !proxy.name_has_owner(name.clone()).map_err(|e| e.to_string())? {
            return Ok(None);
        }
        proxy.get_connection_unix_process_id(name).map(Some).map_err(|e| e.to_string())
    }

    pub fn execute_callback(argv: Vec<String>, cwd: String) -> Result<(), String> {
        let conn = Connection::session().map_err(|e| e.to_string())?;
        conn.call_method(Some(NAME), PATH, Some(INTERFACE), "ExecuteCallback", &(argv, cwd))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(target_os = "linux")]
impl System for RealSystem {
    fn ensure_daemon(&self) -> Result<String, String> {
        crate::daemon::real::ensure_now().describe()
    }
    fn instance_pid(&self) -> Option<u32> {
        dbus::owner_pid().unwrap_or_else(|e| {
            tracing::warn!("cannot ask the session bus about a running copy ({e})");
            None
        })
    }
    fn request_quit(&self) -> Result<(), String> {
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        dbus::execute_callback(vec!["ledger-pos".into(), QUIT_FOR_UPDATE.into()], cwd)
    }
    fn spawn_detached(&self, exe: &Path) -> Result<u32, String> {
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new(exe);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .env_clear()
            .envs(clean_env(std::env::vars(), std::env::var("APPDIR").ok().as_deref()));
        // SAFETY: setsid is async-signal-safe; it detaches the child from our session so it
        // outlives this process.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
        let pid = child.id();
        self.children.lock().expect("children").insert(pid, child);
        Ok(pid)
    }
    fn child_failed(&self, pid: u32) -> Option<String> {
        let mut children = self.children.lock().expect("children");
        let status = children.get_mut(&pid)?.try_wait().ok()??;
        (!status.success()).then(|| format!("it exited with {status}"))
    }
    fn appimage_of(&self, pid: u32) -> Option<PathBuf> {
        let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
        environ
            .split(|b| *b == 0)
            .find_map(|kv| kv.strip_prefix(b"APPIMAGE="))
            .map(|v| PathBuf::from(String::from_utf8_lossy(v).into_owned()))
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// The AppImage update path is Linux-only; elsewhere the engine is never reached.
#[cfg(not(target_os = "linux"))]
impl System for RealSystem {
    fn ensure_daemon(&self) -> Result<String, String> {
        crate::daemon::real::ensure_now().describe()
    }
    fn instance_pid(&self) -> Option<u32> {
        None
    }
    fn request_quit(&self) -> Result<(), String> {
        Err("not supported on this platform".into())
    }
    fn spawn_detached(&self, exe: &Path) -> Result<u32, String> {
        let _ = &self.children;
        std::process::Command::new(exe).spawn().map(|c| c.id()).map_err(|e| e.to_string())
    }
    fn child_failed(&self, _pid: u32) -> Option<String> {
        None
    }
    fn appimage_of(&self, _pid: u32) -> Option<PathBuf> {
        None
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dbus_name_follows_the_identifier() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        let id = conf["identifier"].as_str().unwrap();
        assert_eq!(NAME, format!("{id}.SingleInstance"));
        assert_eq!(PATH, format!("/{}", NAME.replace('.', "/").replace('-', "_")));
    }

    #[cfg(unix)]
    #[test]
    fn scan_finds_this_users_posd_including_a_replaced_binary() {
        let root = std::env::temp_dir().join(format!("ledger-proc-{}", uuid::Uuid::new_v4()));
        let fake = |pid: u32, uid: u32, exe: &str| {
            let d = root.join(pid.to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("status"), format!("Name:\tx\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n")).unwrap();
            std::os::unix::fs::symlink(exe, d.join("exe")).unwrap();
        };
        fake(101, 1000, "/home/u/.local/bin/posd");
        fake(102, 1000, "/home/u/.local/bin/posd (deleted)");
        fake(103, 0, "/usr/bin/posd");
        fake(104, 1000, "/usr/bin/payment-pos-app");
        std::fs::create_dir_all(root.join("self")).unwrap();
        assert_eq!(scan_posd(&root, 1000), vec![101, 102]);
        assert_eq!(
            scan_posd_exe(&root, 1000),
            vec![
                (101, PathBuf::from("/home/u/.local/bin/posd")),
                (102, PathBuf::from("/home/u/.local/bin/posd (deleted)")),
            ]
        );
    }

    #[test]
    fn clean_env_drops_the_appimage_identity_and_our_mount() {
        let vars = vec![
            ("APPIMAGE".to_string(), "/dl/new.AppImage".to_string()),
            ("APPDIR".to_string(), "/tmp/.mount_ab".to_string()),
            ("GDK_PIXBUF_MODULE_FILE".to_string(), "/tmp/.mount_ab/usr/lib/loaders.cache".to_string()),
            ("XDG_DATA_DIRS".to_string(), "/tmp/.mount_ab/usr/share:/usr/share".to_string()),
            ("HOME".to_string(), "/home/u".to_string()),
        ];
        let out = clean_env(vars, Some("/tmp/.mount_ab"));
        assert_eq!(
            out,
            vec![
                ("XDG_DATA_DIRS".to_string(), "/usr/share".to_string()),
                ("HOME".to_string(), "/home/u".to_string()),
            ]
        );
    }
}

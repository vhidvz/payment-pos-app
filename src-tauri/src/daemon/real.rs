//! The real machine behind [`super::Host`], and the choices that depend on it: which `posd`,
//! how it runs, and where its files go.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{ensure, remove, Health, Host, Mode, Paths, Status, Target, Timeouts, DAEMON_SINCE, UNIT};
use crate::install::{Layout, Source};

/// `GET /api/v1/health` on `addr` (`host:port`). `None`: nothing accepts connections there.
pub fn probe(addr: &str, timeout: Duration) -> Option<Health> {
    let sock = addr.to_socket_addrs().ok()?.next()?;
    let mut stream = TcpStream::connect_timeout(&sock, timeout).ok()?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = format!("GET /api/v1/health HTTP/1.0\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return Some(parse_health(b""));
    }
    let mut raw = Vec::new();
    let _ = stream.take(64 * 1024).read_to_end(&mut raw);
    Some(parse_health(&raw))
}

/// What a health response says. A response that is not ours still means the port is taken.
pub fn parse_health(raw: &[u8]) -> Health {
    let stranger = Health { version: String::new(), process: Some("another program".into()) };
    let text = String::from_utf8_lossy(raw);
    let Some((head, body)) = text.split_once("\r\n\r\n") else { return stranger };
    if head.lines().next().and_then(|l| l.split_whitespace().nth(1)) != Some("200") {
        return stranger;
    }
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(body) else { return stranger };
    match doc["version"].as_str() {
        Some(version) => Health {
            version: version.to_string(),
            process: doc["process"].as_str().map(str::to_string),
        },
        None => stranger,
    }
}

/// Where to reach the API: where a live `posd` says it serves (a failed rebind leaves it on
/// the last address that worked), else the configured address. Wildcards become loopback.
pub fn endpoint_from(
    settings: &crate::settings::ServerSettings,
    serving: Option<&crate::server::Serving>,
    alive: &dyn Fn(u32) -> bool,
) -> (String, u16) {
    let (host, port) = match serving.filter(|s| alive(s.pid)) {
        Some(s) => (s.host.clone(), s.port),
        None => (settings.host.clone(), settings.port),
    };
    let host = if host == "0.0.0.0" || host == "::" { "127.0.0.1".to_string() } else { host };
    (host, port)
}

/// [`endpoint_from`] on this machine.
pub fn api_endpoint() -> (String, u16) {
    let settings = crate::settings::peek(&crate::settings::settings_path()).server;
    let serving = crate::server::read_serving(&crate::server::serving_path());
    endpoint_from(&settings, serving.as_ref(), &pid_alive)
}

fn api_addr() -> String {
    let (host, port) = api_endpoint();
    let host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host };
    format!("{host}:{port}")
}

/// Whether `pid` runs — a zombie (exited, not yet reaped by its parent) does not.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        // `/proc/<pid>/stat`: "pid (comm) S …" — the state follows the last ')'.
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().next()) != Some("Z"),
            Err(_) => false,
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    // SAFETY: signal 0 only checks that the process exists.
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// The real machine. Keeps the children it starts, so a child-mode `posd` is reaped.
pub struct RealHost {
    children: Mutex<HashMap<u32, Child>>,
    log: PathBuf,
}

impl RealHost {
    pub fn new() -> Self {
        Self { children: Default::default(), log: Layout::current().state_dir().join("posd.log") }
    }
}

impl Default for RealHost {
    fn default() -> Self {
        Self::new()
    }
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("{program} {} exited with {}", args.join(" "), out.status) } else { err })
    }
}

fn user() -> Option<String> {
    std::env::var("USER").ok().filter(|u| !u.is_empty())
}

impl Host for RealHost {
    fn systemctl(&self, args: &[&str]) -> Result<String, String> {
        let mut all = vec!["--user"];
        all.extend_from_slice(args);
        run("systemctl", &all)
    }

    fn manager_unit_dir(&self) -> Option<PathBuf> {
        if !cfg!(target_os = "linux") {
            return None;
        }
        // The manager's own environment decides where it looks — not ours, which may differ.
        let env = self.systemctl(&["show-environment"]).ok()?;
        let var = |k: &str| env.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(PathBuf::from);
        let config = var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")))?;
        Some(config.join("systemd/user"))
    }

    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn write(&self, path: &Path, body: &str) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let meta = std::fs::metadata(path).ok();
        crate::fsutil::atomic_write(path, body.as_bytes(), meta.as_ref(), 0o644)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    fn remove(&self, path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    fn is_executable(&self, path: &Path) -> bool {
        let Ok(meta) = std::fs::metadata(path) else { return false };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        meta.is_file()
    }

    fn binary_version(&self, exe: &Path) -> Option<String> {
        let out = Command::new(exe).arg("--version").stdin(Stdio::null()).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.trim().strip_prefix("posd ").map(str::to_string)
    }

    fn posd_processes(&self) -> Vec<(u32, PathBuf)> {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: getuid cannot fail and has no preconditions.
            crate::update::system::scan_posd_exe(Path::new("/proc"), unsafe { libc::getuid() })
        }
        #[cfg(not(target_os = "linux"))]
        Vec::new()
    }

    fn terminate(&self, pid: u32) {
        #[cfg(unix)]
        // SAFETY: kill with a pid we found and a valid signal has no memory effects.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = Command::new("taskkill").args(["/PID", &pid.to_string()]).output();
    }

    fn alive(&self, pid: u32) -> bool {
        // Our own child is a zombie until reaped — reap it first, or it reads as alive.
        if let Some(child) = self.children.lock().expect("children").get_mut(&pid) {
            return matches!(child.try_wait(), Ok(None));
        }
        pid_alive(pid)
    }

    fn spawn(&self, exe: &Path, detached: bool) -> Result<u32, String> {
        if let Some(dir) = self.log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let log = std::fs::File::create(&self.log).ok();
        let mut cmd = Command::new(exe);
        cmd.stdin(Stdio::null())
            .stdout(log.as_ref().and_then(|f| f.try_clone().ok()).map_or(Stdio::null(), Stdio::from))
            .stderr(log.map_or(Stdio::null(), Stdio::from))
            // Not our AppImage's identity or mount: posd outlives this process.
            .env_clear()
            .envs(crate::update::system::clean_env(std::env::vars(), std::env::var("APPDIR").ok().as_deref()));
        #[cfg(unix)]
        if detached {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid is async-signal-safe; the child leaves our session and survives us.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        // A child goes with us even when we are killed rather than closed (no exit event).
        // The supervisor thread that spawns it lives as long as the process.
        #[cfg(target_os = "linux")]
        if !detached {
            use std::os::unix::process::CommandExt;
            // SAFETY: prctl(PR_SET_PDEATHSIG) is async-signal-safe and touches no memory.
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            cmd.creation_flags(if detached { CREATE_NO_WINDOW | DETACHED_PROCESS } else { CREATE_NO_WINDOW });
        }
        let child = cmd.spawn().map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
        let pid = child.id();
        self.children.lock().expect("children").insert(pid, child);
        Ok(pid)
    }

    fn health(&self) -> Option<Health> {
        probe(&api_addr(), Duration::from_secs(1))
    }

    fn lingering(&self) -> bool {
        user().is_some_and(|u| Path::new("/var/lib/systemd/linger").join(u).exists())
    }

    fn enable_linger(&self) -> Result<(), String> {
        run("loginctl", &["--no-ask-password", "enable-linger"]).map(|_| ())
    }

    fn disable_linger(&self) {
        let _ = run("loginctl", &["--no-ask-password", "disable-linger"]);
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Where this account's service files go. `LEDGER_POS_DAEMON_UNIT` renames the unit (a test
/// beside a real install); `LEDGER_POS_LINGER=0` leaves logind alone.
pub fn paths() -> Paths {
    // What posd needs to find the same files and network as this process, when the user
    // manager's environment differs from ours.
    const PASS: [&str; 10] = [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "LEDGER_POS_ADMIN_PATH",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
    ];
    Paths {
        unit_name: std::env::var("LEDGER_POS_DAEMON_UNIT").ok().filter(|u| u.ends_with(".service")).unwrap_or_else(|| UNIT.into()),
        autostart: cfg!(target_os = "linux")
            .then(dirs::config_dir)
            .flatten()
            .map(|d| d.join("autostart/ledger-posd.desktop")),
        state_dir: Layout::current().state_dir(),
        env: PASS.iter().filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v))).collect(),
        linger: std::env::var("LEDGER_POS_LINGER").map_or(true, |v| v != "0"),
    }
}

/// `LEDGER_POS_DAEMON` if set; else a debug build keeps `posd` its child (a developer's real
/// systemd is never touched), Linux gets a service, everything else a detached process.
pub fn default_mode() -> Mode {
    match std::env::var("LEDGER_POS_DAEMON").as_deref() {
        Ok("service") => Mode::Service,
        Ok("detached") => Mode::Detached,
        Ok("child") => Mode::Child,
        _ if cfg!(debug_assertions) => Mode::Child,
        _ if cfg!(target_os = "linux") => Mode::Service,
        _ => Mode::Detached,
    }
}

const POSD: &str = if cfg!(windows) { "posd.exe" } else { "posd" };

/// Which `posd` to run. An AppImage runs the installed one; until it is installed (or after
/// an uninstall — `ephemeral`) only the copy inside our mount exists, and it lives only as
/// long as we do, so it runs as our child. Anything else runs the one beside this binary.
pub fn target_for(ephemeral: bool) -> Target {
    let mode = default_mode();
    match Source::detect() {
        Some(source) => {
            let installed = Layout::current().posd_path();
            if !ephemeral && installed.is_file() {
                Target { exe: installed, mode }
            } else {
                Target { exe: source.appdir.join("usr/bin").join(POSD), mode: Mode::Child }
            }
        }
        None => {
            let exe = std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(|d| d.join(POSD)))
                .unwrap_or_else(|| PathBuf::from(POSD));
            Target { exe, mode: if ephemeral { Mode::Child } else { mode } }
        }
    }
}

/// Serialise reconciles across processes (the console, `--install`, the update screen).
fn lock() -> Option<std::fs::File> {
    let dir = Layout::current().state_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".daemon.lock"))
        .ok()?;
    file.lock().ok()?;
    Some(file)
}

/// Reconcile with `host` (the console keeps one, to reap its child).
pub fn ensure_with(host: &RealHost, ephemeral: bool) -> Status {
    let _lock = lock();
    ensure(host, &paths(), &target_for(ephemeral), &Timeouts::default())
}

/// Reconcile once, from a command-line verb or the update screen.
pub fn ensure_now() -> Status {
    ensure_with(&RealHost::new(), false)
}

/// Undo everything [`ensure_now`] set up.
pub fn remove_now() {
    remove_with(&RealHost::new(), &target_for(false).exe);
}

/// [`remove`] under the reconcile lock, stopping the `posd` that runs `exe`.
pub fn remove_with(host: &RealHost, exe: &Path) {
    let _lock = lock();
    remove(host, &paths(), exe);
}

/// After a rollback put `version` back: one from before the service (it serves from its own
/// window) gets none; any other has its service restarted onto it.
pub fn after_rollback(version: &str) -> Option<Status> {
    match semver::Version::parse(version) {
        Ok(v) if (v.major, v.minor) < DAEMON_SINCE => {
            remove_now();
            None
        }
        _ => Some(ensure_now()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn the_endpoint_is_where_a_live_posd_serves() {
        let settings = crate::settings::ServerSettings { port: 8080, ..Default::default() };
        let serving = crate::server::Serving { pid: 42, host: "127.0.0.1".into(), port: 4373, version: "1.2.0".into() };
        assert_eq!(endpoint_from(&settings, Some(&serving), &|p| p == 42), ("127.0.0.1".into(), 4373));
        assert_eq!(endpoint_from(&settings, Some(&serving), &|_| false), ("127.0.0.1".into(), 8080), "a dead pid's file is stale");
        assert_eq!(endpoint_from(&settings, None, &|_| true), ("127.0.0.1".into(), 8080));
        let wild = crate::settings::ServerSettings { host: "0.0.0.0".into(), ..Default::default() };
        assert_eq!(endpoint_from(&wild, None, &|_| true).0, "127.0.0.1", "a wildcard is not connectable");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_zombie_is_not_alive() {
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(300)); // exited, not reaped: a zombie
        assert!(!pid_alive(pid), "a zombie holds no port and serves nothing");
        drop(child);
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn health_parses_an_http_response() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let n = sock.read(&mut buf).unwrap();
            assert!(String::from_utf8_lossy(&buf[..n]).starts_with("GET /api/v1/health HTTP/1."));
            let body = r#"{"status":"ok","version":"1.2.0","process":"posd","uptimeSecs":3}"#;
            write!(sock, "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len()).unwrap();
        });
        let health = probe(&addr, Duration::from_secs(2));
        server.join().unwrap();
        assert_eq!(health, Some(Health { version: "1.2.0".into(), process: Some("posd".into()) }));
    }

    #[test]
    fn nothing_listening_is_none() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert_eq!(probe(&format!("127.0.0.1:{port}"), Duration::from_millis(300)), None);
    }

    #[test]
    fn an_old_server_and_a_stranger_are_told_apart() {
        let old = parse_health(b"HTTP/1.1 200 OK\r\n\r\n{\"status\":\"ok\",\"version\":\"1.1.1\",\"uptimeSecs\":9}");
        assert_eq!(old, Health { version: "1.1.1".into(), process: None });
        let stranger = parse_health(b"HTTP/1.1 404 Not Found\r\n\r\nnope");
        assert_eq!(stranger.process.as_deref(), Some("another program"));
    }
}

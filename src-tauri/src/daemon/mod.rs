//! The background API service: `posd` is the one process that serves the REST API; the
//! console only makes sure it is installed, runs the binary on disk and answers — and
//! undoes that on uninstall. Every side effect goes through [`Host`], so each branch is
//! tested against a script.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

pub mod real;
pub mod supervisor;

/// The unit's name — the one the 1.1 guide had people write by hand, so a hand-written
/// unit is taken over instead of a second server being added beside it.
pub const UNIT: &str = "posd.service";
/// First line of every unit we write; [`remove`] deletes only files that carry it.
pub const MARKER: &str = "# Managed by Ledger POS — rewritten by the app; edit settings in the app, not here.";
/// Versions before this serve the API from their own window and know nothing of a service.
pub const DAEMON_SINCE: (u64, u64) = (1, 2);
/// Present while linger is on because we turned it on — so removal undoes only our doing.
const LINGER_MARK: &str = "linger-enabled-by-ledger-pos";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A systemd user unit: started at login (at boot with linger), restarted on a crash.
    Service,
    /// Started by us, detached; at login through an XDG autostart entry.
    Detached,
    /// A child of the console, stopped with it — debug builds and the last session after an
    /// uninstall.
    Child,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Detached => "detached",
            Self::Child => "child",
        }
    }
}

/// Which `posd` to run, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub exe: PathBuf,
    pub mode: Mode,
}

/// What `GET /api/v1/health` said on the configured address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub version: String,
    /// Absent before 1.2 — the server may be an old window or an old `posd`.
    pub process: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// `service`, `detached` or `child`.
    pub mode: &'static str,
    /// `starting`, `serving` or `failed`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The process this call started, when it started one (detached/child).
    #[serde(skip)]
    pub spawned: Option<u32>,
}

impl Status {
    /// One line for a log or a progress step: `Err` only when it failed outright — a
    /// service still starting is on its way.
    pub fn describe(&self) -> Result<String, String> {
        let line = match (&self.version, &self.detail) {
            (Some(v), _) => format!("{} {} ({v})", self.mode, self.state),
            (None, Some(d)) => format!("{} {} — {d}", self.mode, self.state),
            (None, None) => format!("{} {}", self.mode, self.state),
        };
        if self.state == "failed" { Err(line) } else { Ok(line) }
    }
}

/// Where this account's service files live.
#[derive(Debug, Clone)]
pub struct Paths {
    pub unit_name: String,
    /// The XDG autostart entry for detached mode (Linux only).
    pub autostart: Option<PathBuf>,
    /// The installer's state directory (`~/.local/share/ledger-pos`).
    pub state_dir: PathBuf,
    /// Variables the service needs to find the same files as this process.
    pub env: Vec<(String, String)>,
    /// Ask logind to start the service at boot, not only at login.
    pub linger: bool,
}

pub struct Timeouts {
    pub healthy_within: Duration,
    /// Longer than `posd`'s 300 s drain cap: a stop waits for the payment in flight.
    pub stop_cap: Duration,
    pub poll: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            healthy_within: Duration::from_secs(20),
            stop_cap: Duration::from_secs(330),
            poll: Duration::from_millis(250),
        }
    }
}

/// The machine the reconcile acts on.
pub trait Host: Send + Sync {
    /// `systemctl --user <args>`; `Ok(stdout)` on success.
    fn systemctl(&self, args: &[&str]) -> Result<String, String>;
    /// The user manager's own unit directory; `None` when there is no systemd user manager.
    fn manager_unit_dir(&self) -> Option<PathBuf>;
    fn read(&self, path: &Path) -> Option<String>;
    fn write(&self, path: &Path, body: &str) -> Result<(), String>;
    fn remove(&self, path: &Path);
    fn is_executable(&self, path: &Path) -> bool;
    /// What `<exe> --version` says, without the `posd ` prefix.
    fn binary_version(&self, exe: &Path) -> Option<String>;
    /// This user's `posd` processes and the binary each runs.
    fn posd_processes(&self) -> Vec<(u32, PathBuf)>;
    /// SIGTERM: `posd` drains, then exits.
    fn terminate(&self, pid: u32);
    fn alive(&self, pid: u32) -> bool;
    fn spawn(&self, exe: &Path, detached: bool) -> Result<u32, String>;
    fn health(&self) -> Option<Health>;
    fn lingering(&self) -> bool;
    fn enable_linger(&self) -> Result<(), String>;
    fn disable_linger(&self);
    fn sleep(&self, d: Duration);
    fn now(&self) -> Instant;
}

/// systemd expands `%` specifiers in every value; `%%` is a literal one.
fn unit_escape(s: &str) -> String {
    s.replace('%', "%%")
}

/// The unit file for `exe`.
pub fn render_unit(exe: &Path, env: &[(String, String)]) -> String {
    let path = exe.display().to_string();
    // ExecStart also expands `$VAR`, and takes a double-quoted word with C escapes.
    let exec = unit_escape(&path).replace('\\', "\\\\").replace('"', "\\\"").replace('$', "$$");
    let mut environment = String::new();
    for (k, v) in env {
        let kv = unit_escape(&format!("{k}={v}")).replace('\\', "\\\\").replace('"', "\\\"");
        environment.push_str(&format!("Environment=\"{kv}\"\n"));
    }
    format!(
        "{MARKER}\n\
         [Unit]\n\
         Description=Ledger POS REST API (posd)\n\
         # A binary that is gone (package removed) skips the start instead of looping.\n\
         ConditionFileIsExecutable={cond}\n\
         StartLimitIntervalSec=120\n\
         StartLimitBurst=10\n\
         \n\
         [Service]\n\
         ExecStart=\"{exec}\"\n\
         {environment}\
         Restart=always\n\
         RestartSec=3\n\
         # posd finishes the payment in flight before it exits (300 s cap).\n\
         TimeoutStopSec=330\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        cond = unit_escape(&path),
    )
}

/// The XDG autostart entry that starts a detached `posd` at login.
fn autostart_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name=Ledger POS API\n\
         Comment=Ledger POS — the REST API, in the background\n\
         Exec={}\n\
         NoDisplay=true\n\
         StartupNotify=false\n\
         Terminal=false\n",
        crate::install::quote_exec(exe)
    )
}

/// `posd` of the version on disk answers — or, when the binary cannot say, any `posd`.
fn healthy(health: &Option<Health>, expected: &Option<String>) -> bool {
    match health {
        Some(h) if h.process.as_deref() == Some("posd") => expected.as_ref().is_none_or(|v| *v == h.version),
        _ => false,
    }
}

fn status(mode: Mode, state: &'static str, version: Option<String>, detail: Option<String>) -> Status {
    Status { mode: mode.label(), state, version, detail, spawned: None }
}

fn describe(health: &Option<Health>) -> String {
    match health {
        Some(Health { version, process: Some(p) }) => format!("the API port is answered by {p} {version}"),
        Some(Health { version, process: None }) => {
            format!("the API port is held by an older Ledger POS ({version}) — it is released when that copy closes")
        }
        None => "nothing answers on the API port yet".into(),
    }
}

/// Poll health until the expected `posd` answers, or the cap runs out.
fn wait_healthy(host: &dyn Host, mode: Mode, expected: &Option<String>, t: &Timeouts) -> Status {
    let started = host.now();
    loop {
        let health = host.health();
        if healthy(&health, expected) {
            let version = health.map(|h| h.version);
            return status(mode, "serving", version, None);
        }
        if host.now() - started >= t.healthy_within {
            return status(mode, "starting", None, Some(describe(&health)));
        }
        host.sleep(t.poll);
    }
}

/// SIGTERM each pid and wait (they drain) until all are gone or the cap runs out.
fn retire(host: &dyn Host, pids: &[u32], t: &Timeouts) {
    if pids.is_empty() {
        return;
    }
    for pid in pids {
        tracing::info!("stopping posd pid {pid}; it finishes its requests first");
        host.terminate(*pid);
    }
    let started = host.now();
    while pids.iter().any(|p| host.alive(*p)) && host.now() - started < t.stop_cap {
        host.sleep(t.poll);
    }
}

fn same_binary(running: &Path, exe: &Path) -> bool {
    let running = running.to_string_lossy();
    let running = running.strip_suffix(" (deleted)").unwrap_or(&running);
    Path::new(running) == exe
}

/// Make the API answer from `target`, idempotently.
pub fn ensure(host: &dyn Host, paths: &Paths, target: &Target, t: &Timeouts) -> Status {
    if !host.is_executable(&target.exe) {
        return status(
            target.mode,
            "failed",
            None,
            Some(format!("{} is missing or not executable", target.exe.display())),
        );
    }
    let expected = host.binary_version(&target.exe);
    match target.mode {
        Mode::Service => match ensure_service(host, paths, target, &expected, t) {
            Ok(s) => s,
            Err(why) => {
                tracing::warn!("cannot run the API as a systemd user service ({why}); starting it directly");
                let mut s = ensure_spawned(host, paths, &target.exe, &expected, true, t);
                let note = format!("systemd user services are unavailable here ({why}); the API runs without one");
                s.detail = Some(match s.detail.take() {
                    Some(d) => format!("{note}; {d}"),
                    None => note,
                });
                s
            }
        },
        Mode::Detached => ensure_spawned(host, paths, &target.exe, &expected, true, t),
        Mode::Child => ensure_spawned(host, paths, &target.exe, &expected, false, t),
    }
}

fn ensure_service(
    host: &dyn Host,
    paths: &Paths,
    target: &Target,
    expected: &Option<String>,
    t: &Timeouts,
) -> Result<Status, String> {
    let dir = host.manager_unit_dir().ok_or("no systemd user manager")?;
    let unit = paths.unit_name.as_str();
    let unit_path = dir.join(unit);
    let existing = host.read(&unit_path);
    let body = render_unit(&target.exe, &unit_env(&paths.env, existing.as_deref()));
    let changed = existing.as_deref() != Some(body.as_str());
    if changed {
        host.write(&unit_path, &body)?;
        host.systemctl(&["daemon-reload"])?;
    }
    host.systemctl(&["enable", unit])?;
    // Leaving detached mode: its login entry would start a second copy.
    if let Some(entry) = &paths.autostart {
        host.remove(entry);
    }
    linger(host, paths);

    let before = host.health();
    if healthy(&before, expected) {
        host.systemctl(&["start", unit])?;
        return Ok(wait_healthy(host, Mode::Service, expected, t));
    }
    let main: u32 = host
        .systemctl(&["show", "-p", "MainPID", "--value", unit])
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let procs = host.posd_processes();
    if before.is_some() {
        // Something wrong answers: a `posd` outside the unit may be holding the port.
        let strays: Vec<u32> = procs.iter().map(|(p, _)| *p).filter(|p| *p != main).collect();
        retire(host, &strays, t);
    }
    // The unit's own posd runs the binary on disk (`(deleted)` once replaced) under this
    // unit: a restart gains nothing — it is starting, or the port is another program's — and
    // would throw away the last address it could bind.
    let current = main != 0 && procs.iter().any(|(p, running)| *p == main && running.as_path() == target.exe);
    if changed || !current {
        let _ = host.systemctl(&["reset-failed", unit]);
        host.systemctl(&["restart", unit])?;
    } else {
        host.systemctl(&["start", unit])?;
    }
    Ok(wait_healthy(host, Mode::Service, expected, t))
}

fn ensure_spawned(
    host: &dyn Host,
    paths: &Paths,
    exe: &Path,
    expected: &Option<String>,
    detached: bool,
    t: &Timeouts,
) -> Status {
    let mode = if detached { Mode::Detached } else { Mode::Child };
    if detached {
        if let Some(entry) = &paths.autostart {
            let body = autostart_entry(exe);
            if host.read(entry).as_deref() != Some(body.as_str()) {
                if let Err(e) = host.write(entry, &body) {
                    tracing::warn!("cannot write {} ({e}); the API will not start at login", entry.display());
                }
            }
        }
    }
    let health = host.health();
    if healthy(&health, expected) {
        return status(mode, "serving", health.map(|h| h.version), None);
    }
    let procs = host.posd_processes();
    if matches!(&health, Some(h) if h.process.as_deref() == Some("posd")) {
        // A posd of the wrong version answers: replace every posd we can.
        retire(host, &procs.iter().map(|(p, _)| *p).collect::<Vec<_>>(), t);
    } else if procs.iter().any(|(_, running)| same_binary(running, exe)) {
        // Ours is up but not answering — still starting, or waiting for a port another
        // program (or an older window) holds; killing posd would free nothing.
        return status(mode, "starting", None, Some(describe(&health)));
    }
    let pid = match host.spawn(exe, detached) {
        Ok(pid) => pid,
        Err(e) => return status(mode, "failed", None, Some(e)),
    };
    let mut s = wait_healthy(host, mode, expected, t);
    s.spawned = Some(pid);
    s
}

/// How the service is installed now, whatever this build would choose: a unit we wrote,
/// a login entry for a detached posd, or nothing.
pub fn mode_in_use(host: &dyn Host, paths: &Paths) -> Option<Mode> {
    if let Some(dir) = host.manager_unit_dir() {
        if host.read(&dir.join(&paths.unit_name)).is_some_and(|b| b.starts_with(MARKER)) {
            return Some(Mode::Service);
        }
    }
    paths.autostart.as_ref().filter(|a| host.read(a).is_some()).map(|_| Mode::Detached)
}

/// Network settings a process may lack — a console started from the menu, without the
/// terminal's proxy — that must not be stripped from the unit (webhooks need them) nor
/// rewrite it and restart posd: a process that has one sets it; one without leaves it.
const STICKY: [&str; 6] = ["HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy"];

/// The `Environment=` pairs of a unit we rendered.
fn parse_unit_env(unit: &str) -> Vec<(String, String)> {
    unit.lines()
        .filter_map(|l| l.strip_prefix("Environment=\"")?.strip_suffix('"'))
        .filter_map(|quoted| {
            let mut out = String::new();
            let mut chars = quoted.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => out.extend(chars.next()),
                    '%' => {
                        out.push('%');
                        chars.next(); // `%%` is one literal `%`
                    }
                    c => out.push(c),
                }
            }
            out.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// The unit's environment: this process's, plus sticky settings the unit already carries.
/// Sorted, so the same settings always render the same file.
fn unit_env(current: &[(String, String)], existing: Option<&str>) -> Vec<(String, String)> {
    let mut env = current.to_vec();
    for (k, v) in existing.map(parse_unit_env).unwrap_or_default() {
        if STICKY.contains(&k.as_str()) && !env.iter().any(|(have, _)| *have == k) {
            env.push((k, v));
        }
    }
    env.sort();
    env
}

/// Turn linger on once, remembering that we did.
fn linger(host: &dyn Host, paths: &Paths) {
    if !paths.linger || host.lingering() {
        return;
    }
    match host.enable_linger() {
        Ok(()) => {
            let _ = host.write(&paths.state_dir.join(LINGER_MARK), "");
        }
        Err(e) => tracing::info!("the API starts at login, not at boot: linger was refused ({e})"),
    }
}

/// Undo [`ensure`]: stop and forget the service, the autostart entry and our linger, and
/// stop every `posd` running `exe`.
pub fn remove(host: &dyn Host, paths: &Paths, exe: &Path) {
    if let Some(dir) = host.manager_unit_dir() {
        let unit_path = dir.join(&paths.unit_name);
        if host.read(&unit_path).is_some_and(|body| body.starts_with(MARKER)) {
            let _ = host.systemctl(&["disable", "--now", &paths.unit_name]);
            host.remove(&unit_path);
            let _ = host.systemctl(&["daemon-reload"]);
        }
    }
    if let Some(entry) = &paths.autostart {
        host.remove(entry);
    }
    host.remove(&paths.state_dir.join("posd.log"));
    let mark = paths.state_dir.join(LINGER_MARK);
    if host.read(&mark).is_some() {
        host.disable_linger();
        host.remove(&mark);
    }
    let ours: Vec<u32> =
        host.posd_processes().into_iter().filter(|(_, running)| same_binary(running, exe)).map(|(p, _)| p).collect();
    retire(host, &ours, &Timeouts::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const EXE: &str = "/home/u/.local/bin/posd";
    const UNIT_DIR: &str = "/home/u/.config/systemd/user";

    /// A scripted machine. Time is virtual; `start`/`restart`/`spawn` make `serves` answer.
    struct Fake {
        base: Instant,
        s: Mutex<St>,
    }
    struct St {
        clock: Duration,
        unit_dir: Option<PathBuf>,
        files: HashMap<PathBuf, String>,
        calls: Vec<String>,
        systemctl_fails: bool,
        main_pid: u32,
        procs: Vec<(u32, PathBuf)>,
        terminated: Vec<u32>,
        versions: HashMap<PathBuf, String>,
        /// What answers on the port now.
        serving: Option<Health>,
        /// Whether a start/spawn makes the target answer.
        starts: bool,
        spawned: Vec<(PathBuf, bool)>,
        lingering: bool,
        linger_calls: Vec<&'static str>,
        next_pid: u32,
    }

    impl Fake {
        fn new() -> Self {
            let mut versions = HashMap::new();
            versions.insert(PathBuf::from(EXE), "1.2.0".to_string());
            Self {
                base: Instant::now(),
                s: Mutex::new(St {
                    clock: Duration::ZERO,
                    unit_dir: Some(PathBuf::from(UNIT_DIR)),
                    files: HashMap::new(),
                    calls: vec![],
                    systemctl_fails: false,
                    main_pid: 0,
                    procs: vec![],
                    terminated: vec![],
                    versions,
                    serving: None,
                    starts: true,
                    spawned: vec![],
                    lingering: false,
                    linger_calls: vec![],
                    next_pid: 500,
                }),
            }
        }
        fn with(self, f: impl FnOnce(&mut St)) -> Self {
            f(&mut self.s.lock().unwrap());
            self
        }
        fn st(&self) -> std::sync::MutexGuard<'_, St> {
            self.s.lock().unwrap()
        }
        fn answer(s: &mut St, exe: &Path) {
            if s.starts {
                let version = s.versions.get(exe).cloned().unwrap_or_default();
                s.serving = Some(Health { version, process: Some("posd".into()) });
            }
        }
        fn unit_path() -> PathBuf {
            PathBuf::from(UNIT_DIR).join(UNIT)
        }
    }

    impl Host for Fake {
        fn systemctl(&self, args: &[&str]) -> Result<String, String> {
            let mut s = self.st();
            if s.systemctl_fails {
                return Err("Failed to connect to bus".into());
            }
            let line = args.join(" ");
            let out = match args {
                ["show", "-p", "MainPID", "--value", _] => s.main_pid.to_string(),
                ["start", _] | ["restart", _] => {
                    let exe = PathBuf::from(EXE);
                    Fake::answer(&mut s, &exe);
                    String::new()
                }
                _ => String::new(),
            };
            if !line.starts_with("show") {
                s.calls.push(line);
            }
            Ok(out)
        }
        fn manager_unit_dir(&self) -> Option<PathBuf> {
            self.st().unit_dir.clone()
        }
        fn read(&self, path: &Path) -> Option<String> {
            self.st().files.get(path).cloned()
        }
        fn write(&self, path: &Path, body: &str) -> Result<(), String> {
            self.st().files.insert(path.to_path_buf(), body.to_string());
            Ok(())
        }
        fn remove(&self, path: &Path) {
            self.st().files.remove(path);
        }
        fn is_executable(&self, path: &Path) -> bool {
            self.st().versions.contains_key(path)
        }
        fn binary_version(&self, exe: &Path) -> Option<String> {
            self.st().versions.get(exe).cloned()
        }
        fn posd_processes(&self) -> Vec<(u32, PathBuf)> {
            self.st().procs.clone()
        }
        fn terminate(&self, pid: u32) {
            let mut s = self.st();
            s.terminated.push(pid);
            s.procs.retain(|(p, _)| *p != pid);
            s.serving = None;
        }
        fn alive(&self, pid: u32) -> bool {
            self.st().procs.iter().any(|(p, _)| *p == pid)
        }
        fn spawn(&self, exe: &Path, detached: bool) -> Result<u32, String> {
            let mut s = self.st();
            s.spawned.push((exe.to_path_buf(), detached));
            s.next_pid += 1;
            let pid = s.next_pid;
            s.procs.push((pid, exe.to_path_buf()));
            Fake::answer(&mut s, exe);
            Ok(pid)
        }
        fn health(&self) -> Option<Health> {
            self.st().serving.clone()
        }
        fn lingering(&self) -> bool {
            self.st().lingering
        }
        fn enable_linger(&self) -> Result<(), String> {
            let mut s = self.st();
            s.linger_calls.push("enable");
            s.lingering = true;
            Ok(())
        }
        fn disable_linger(&self) {
            let mut s = self.st();
            s.linger_calls.push("disable");
            s.lingering = false;
        }
        fn sleep(&self, d: Duration) {
            self.st().clock += d;
        }
        fn now(&self) -> Instant {
            self.base + self.st().clock
        }
    }

    fn paths() -> Paths {
        Paths {
            unit_name: UNIT.into(),
            autostart: Some(PathBuf::from("/home/u/.config/autostart/ledger-posd.desktop")),
            state_dir: PathBuf::from("/home/u/.local/share/ledger-pos"),
            env: vec![],
            linger: true,
        }
    }

    fn target(mode: Mode) -> Target {
        Target { exe: PathBuf::from(EXE), mode }
    }

    fn go(host: &Fake, mode: Mode) -> Status {
        ensure(host, &paths(), &target(mode), &Timeouts::default())
    }

    fn posd(version: &str) -> Option<Health> {
        Some(Health { version: version.into(), process: Some("posd".into()) })
    }

    #[test]
    fn exec_start_is_quoted_and_escaped() {
        let unit = render_unit(
            Path::new("/home/a b/%x$y/posd"),
            &[("XDG_CONFIG_HOME".into(), "/t/c".into())],
        );
        assert!(unit.starts_with(MARKER), "{unit}");
        assert!(unit.contains("ExecStart=\"/home/a b/%%x$$y/posd\"\n"), "{unit}");
        assert!(unit.contains("ConditionFileIsExecutable=/home/a b/%%x$y/posd\n"), "{unit}");
        assert!(unit.contains("Environment=\"XDG_CONFIG_HOME=/t/c\"\n"), "{unit}");
        for line in [
            "Restart=always",
            "RestartSec=3",
            "TimeoutStopSec=330",
            "StartLimitIntervalSec=120",
            "StartLimitBurst=10",
            "WantedBy=default.target",
        ] {
            assert!(unit.lines().any(|l| l == line), "missing {line}: {unit}");
        }
    }

    #[test]
    fn first_ensure_writes_reloads_enables_and_restarts() {
        let host = Fake::new();
        let status = go(&host, Mode::Service);
        assert_eq!((status.mode, status.state, status.version.as_deref()), ("service", "serving", Some("1.2.0")));
        let s = host.st();
        assert_eq!(s.files.get(&Fake::unit_path()), Some(&render_unit(Path::new(EXE), &[])));
        assert_eq!(s.calls, vec!["daemon-reload", "enable posd.service", "reset-failed posd.service", "restart posd.service"]);
    }

    #[test]
    fn an_unchanged_healthy_service_is_only_started() {
        let host = Fake::new();
        go(&host, Mode::Service);
        host.st().calls.clear();
        let status = go(&host, Mode::Service);
        assert_eq!(status.state, "serving");
        assert_eq!(host.st().calls, vec!["enable posd.service", "start posd.service"]);
    }

    #[test]
    fn a_stray_on_the_port_is_retired_and_the_units_posd_kept() {
        let host = Fake::new().with(|s| {
            s.files.insert(Fake::unit_path(), render_unit(Path::new(EXE), &[]));
            s.main_pid = 10;
            s.procs = vec![(10, PathBuf::from(EXE)), (11, PathBuf::from("/opt/old/posd"))];
            s.serving = posd("1.1.1");
        });
        let status = go(&host, Mode::Service);
        assert_eq!((status.state, status.version.as_deref()), ("serving", Some("1.2.0")));
        let s = host.st();
        assert_eq!(s.terminated, vec![11], "the stray holding the port goes");
        assert!(!s.calls.iter().any(|c| c.starts_with("restart")), "the unit's own current posd binds next: {:?}", s.calls);
    }

    /// A posd running the binary on disk under an unchanged unit gains nothing from a restart
    /// — it is starting, or the port is someone else's — and a restart would drop the last
    /// address it could bind.
    #[test]
    fn a_current_posd_that_does_not_answer_is_not_restarted() {
        let host = Fake::new().with(|s| {
            s.files.insert(Fake::unit_path(), render_unit(Path::new(EXE), &[]));
            s.main_pid = 10;
            s.procs = vec![(10, PathBuf::from(EXE))];
            s.starts = false;
        });
        let status = go(&host, Mode::Service);
        assert_eq!(status.state, "starting");
        assert!(!host.st().calls.iter().any(|c| c.starts_with("restart")), "{:?}", host.st().calls);
    }

    #[test]
    fn a_replaced_binary_is_restarted() {
        let host = Fake::new().with(|s| {
            s.files.insert(Fake::unit_path(), render_unit(Path::new(EXE), &[]));
            s.main_pid = 10;
            s.procs = vec![(10, PathBuf::from(format!("{EXE} (deleted)")))];
        });
        let status = go(&host, Mode::Service);
        assert_eq!(status.state, "serving");
        assert!(host.st().calls.contains(&"restart posd.service".to_string()), "{:?}", host.st().calls);
    }

    #[test]
    fn a_healthy_port_leaves_strays_alone() {
        let host = Fake::new().with(|s| {
            s.files.insert(Fake::unit_path(), render_unit(Path::new(EXE), &[]));
            s.main_pid = 10;
            s.procs = vec![(10, PathBuf::from(EXE)), (12, PathBuf::from("/tmp/dev/posd"))];
            s.serving = posd("1.2.0");
        });
        go(&host, Mode::Service);
        assert!(host.st().terminated.is_empty(), "a posd on a spare port is somebody's test");
    }

    #[test]
    fn a_missing_target_fails_without_touching_systemd() {
        let host = Fake::new().with(|s| s.versions.clear());
        let status = go(&host, Mode::Service);
        assert_eq!(status.state, "failed");
        assert!(status.detail.unwrap().contains(EXE));
        assert!(host.st().calls.is_empty());
    }

    #[test]
    fn service_failure_falls_back_to_detached() {
        let host = Fake::new().with(|s| s.systemctl_fails = true);
        let status = go(&host, Mode::Service);
        assert_eq!((status.mode, status.state), ("detached", "serving"));
        assert!(status.detail.as_deref().unwrap_or("").contains("systemd"), "{status:?}");
        assert_eq!(host.st().spawned, vec![(PathBuf::from(EXE), true)]);
    }

    #[test]
    fn no_user_manager_falls_back_to_detached() {
        let host = Fake::new().with(|s| s.unit_dir = None);
        assert_eq!(go(&host, Mode::Service).mode, "detached");
    }

    #[test]
    fn expected_version_is_the_targets_not_the_apps() {
        // A rollback put 1.2.0 back while a newer console runs: serving it is healthy.
        let host = Fake::new().with(|s| {
            s.files.insert(Fake::unit_path(), render_unit(Path::new(EXE), &[]));
            s.serving = posd("1.2.0");
        });
        go(&host, Mode::Service);
        assert!(!host.st().calls.iter().any(|c| c.starts_with("restart")), "{:?}", host.st().calls);
    }

    #[test]
    fn an_old_window_on_the_port_is_not_healthy() {
        let host = Fake::new().with(|s| {
            s.files.insert(Fake::unit_path(), render_unit(Path::new(EXE), &[]));
            s.serving = Some(Health { version: "1.2.0".into(), process: None });
            s.starts = false;
        });
        let status = go(&host, Mode::Service);
        assert_eq!(status.state, "starting", "{status:?}");
        assert!(status.detail.is_some());
    }

    #[test]
    fn detached_spawns_when_unhealthy_and_writes_autostart() {
        let host = Fake::new();
        let status = go(&host, Mode::Detached);
        assert_eq!((status.mode, status.state), ("detached", "serving"));
        assert!(status.spawned.is_some());
        let s = host.st();
        let entry = s.files.get(&paths().autostart.unwrap()).cloned().unwrap();
        assert!(entry.contains(&format!("Exec=\"{EXE}\"")), "{entry}");
        assert_eq!(s.spawned, vec![(PathBuf::from(EXE), true)]);
    }

    #[test]
    fn detached_does_not_spawn_a_second_copy_while_one_waits_for_the_port() {
        let host = Fake::new().with(|s| {
            s.procs = vec![(77, PathBuf::from(EXE))];
            s.starts = false;
        });
        let status = go(&host, Mode::Detached);
        assert_eq!(status.state, "starting");
        assert!(host.st().spawned.is_empty());
    }

    #[test]
    fn child_mode_spawns_attached_and_writes_nothing() {
        let host = Fake::new();
        let status = go(&host, Mode::Child);
        assert_eq!((status.mode, status.state), ("child", "serving"));
        let s = host.st();
        assert_eq!(s.spawned, vec![(PathBuf::from(EXE), false)]);
        assert!(s.files.is_empty() && s.calls.is_empty());
    }

    #[test]
    fn remove_disables_and_deletes_only_ours() {
        let host = Fake::new();
        go(&host, Mode::Service);
        host.st().procs = vec![(10, PathBuf::from(EXE)), (12, PathBuf::from("/tmp/dev/posd"))];
        host.st().calls.clear();
        remove(&host, &paths(), Path::new(EXE));
        let s = host.st();
        assert!(!s.files.contains_key(&Fake::unit_path()));
        assert_eq!(s.calls, vec!["disable --now posd.service", "daemon-reload"]);
        assert_eq!(s.terminated, vec![10]);

        let foreign = Fake::new().with(|s| {
            s.files.insert(Fake::unit_path(), "[Service]\nExecStart=/usr/bin/other\n".into());
        });
        remove(&foreign, &paths(), Path::new(EXE));
        assert!(foreign.st().files.contains_key(&Fake::unit_path()), "a unit we did not write stays");
    }

    /// A console started from the menu lacks the proxy a terminal had: it must not strip it
    /// (webhooks need it) nor rewrite the unit and restart posd over it.
    #[test]
    fn proxy_settings_survive_a_reconcile_without_them() {
        let host = Fake::new();
        let with = Paths { env: vec![("HTTPS_PROXY".into(), "http://p:8080".into())], ..paths() };
        ensure(&host, &with, &target(Mode::Service), &Timeouts::default());
        host.st().calls.clear();
        ensure(&host, &paths(), &target(Mode::Service), &Timeouts::default());
        let s = host.st();
        assert!(s.files[&Fake::unit_path()].contains("Environment=\"HTTPS_PROXY=http://p:8080\""));
        assert_eq!(s.calls, vec!["enable posd.service", "start posd.service"], "no rewrite, no restart");
    }

    #[test]
    fn a_stranger_on_the_port_is_not_fought_in_detached_mode() {
        let stranger = Some(Health { version: String::new(), process: Some("another program".into()) });
        let host = Fake::new().with(|s| {
            s.serving = stranger.clone();
            s.starts = false;
            s.procs = vec![(12, PathBuf::from("/tmp/dev/posd"))];
        });
        assert_eq!(go(&host, Mode::Detached).state, "starting");
        assert_eq!(go(&host, Mode::Detached).state, "starting");
        let s = host.st();
        assert_eq!(s.spawned.len(), 1, "one copy waits for the port; no respawn per tick");
        assert!(s.terminated.is_empty(), "killing posd frees nothing another program holds");
    }

    #[test]
    fn remove_clears_the_daemon_log() {
        let host = Fake::new().with(|s| {
            s.files.insert(paths().state_dir.join("posd.log"), "old".into());
        });
        remove(&host, &paths(), Path::new(EXE));
        assert!(!host.st().files.contains_key(&paths().state_dir.join("posd.log")));
    }

    #[test]
    fn the_mode_in_use_is_what_is_installed() {
        let host = Fake::new();
        assert_eq!(mode_in_use(&host, &paths()), None);
        go(&host, Mode::Detached);
        assert_eq!(mode_in_use(&host, &paths()), Some(Mode::Detached));
        go(&host, Mode::Service);
        assert_eq!(mode_in_use(&host, &paths()), Some(Mode::Service));
    }

    #[test]
    fn linger_is_enabled_once_and_undone_only_if_we_did_it() {
        let host = Fake::new();
        go(&host, Mode::Service);
        go(&host, Mode::Service);
        remove(&host, &paths(), Path::new(EXE));
        assert_eq!(host.st().linger_calls, vec!["enable", "disable"]);

        let theirs = Fake::new().with(|s| s.lingering = true);
        go(&theirs, Mode::Service);
        remove(&theirs, &paths(), Path::new(EXE));
        assert!(theirs.st().linger_calls.is_empty(), "linger the user set is theirs");

        let off = Paths { linger: false, ..paths() };
        let quiet = Fake::new();
        ensure(&quiet, &off, &target(Mode::Service), &Timeouts::default());
        assert!(quiet.st().linger_calls.is_empty());
    }
}

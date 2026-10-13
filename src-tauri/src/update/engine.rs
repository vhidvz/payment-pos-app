//! The update engine (spec §3.3): no running `posd` → install transaction → drain and stop
//! the running copy → launch and verify the new one.

use std::time::Duration;

use serde::Serialize;

use super::same_file;
use super::system::System;
use crate::install::{self, Installed, Layout, Source};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StepId {
    Preflight,
    Install,
    StopRunning,
    Launch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StepState {
    Running,
    Waiting,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub step: StepId,
    pub state: StepState,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EngineOutcome {
    Started { pid: u32 },
    PosdRunning { pids: Vec<u32> },
    RunningNotStopped,
    LaunchFailed { reason: String },
    InstallFailed { reason: String },
}

pub struct Timeouts {
    pub legacy_hint_after: Duration,
    pub stop_cap: Duration,
    pub launch_cap: Duration,
    pub poll: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            legacy_hint_after: Duration::from_secs(10),
            stop_cap: Duration::from_secs(300),
            launch_cap: Duration::from_secs(20),
            poll: Duration::from_millis(250),
        }
    }
}

pub fn posd_message(pids: &[u32]) -> String {
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
    format!(
        "posd is running (pid {list}). Stop it first — for a service, `systemctl --user stop <its unit>` — \
         then retry; started again afterwards, it runs the new version."
    )
}

impl EngineOutcome {
    pub fn describe(&self) -> String {
        match self {
            Self::Started { pid } => format!("the new version is running (pid {pid})"),
            Self::PosdRunning { pids } => posd_message(pids),
            Self::RunningNotStopped => "the new version is installed, but the running copy did not close; \
                it keeps running the old version until it is restarted"
                .into(),
            Self::LaunchFailed { reason } => format!("the new version was installed but did not start: {reason}"),
            Self::InstallFailed { reason } => format!("nothing was changed: {reason}"),
        }
    }
}

fn say(progress: &mut dyn FnMut(Progress), step: StepId, state: StepState, message: impl Into<String>) {
    progress(Progress { step, state, message: message.into() });
}

pub fn run_update(
    sys: &dyn System,
    layout: &Layout,
    source: &Source,
    installed: &Installed,
    t: &Timeouts,
    progress: &mut dyn FnMut(Progress),
) -> EngineOutcome {
    // 0. No running posd: its last write must precede our first migration.
    say(progress, StepId::Preflight, StepState::Running, "Checking that posd is not running");
    let pids = sys.posd_pids();
    if !pids.is_empty() {
        say(progress, StepId::Preflight, StepState::Failed, posd_message(&pids));
        return EngineOutcome::PosdRunning { pids };
    }
    say(progress, StepId::Preflight, StepState::Done, "posd is not running");

    // 1. The install transaction.
    say(progress, StepId::Install, StepState::Running, "Replacing the installed copy");
    if let Err(reason) = install::install(layout, source) {
        say(progress, StepId::Install, StepState::Failed, reason.clone());
        return EngineOutcome::InstallFailed { reason };
    }
    say(progress, StepId::Install, StepState::Done, "Installed");

    // 2. Stop the running copy, letting it finish its transaction.
    if sys.instance_pid().is_some() {
        say(progress, StepId::StopRunning, StepState::Running, "Asking the running copy to finish its work and close");
        if let Err(e) = sys.request_quit() {
            tracing::warn!("could not ask the running copy to close ({e})");
        }
        let started = sys.now();
        let mut hinted = false;
        while sys.instance_pid().is_some() {
            let waited = sys.now() - started;
            if waited >= t.stop_cap {
                say(progress, StepId::StopRunning, StepState::Failed, EngineOutcome::RunningNotStopped.describe());
                return EngineOutcome::RunningNotStopped;
            }
            if !hinted && waited >= t.legacy_hint_after {
                hinted = true;
                let hint = if matches!(installed, Installed::Legacy) {
                    "This version cannot close itself — choose Quit from its tray icon"
                } else {
                    "Waiting for the running copy to finish its transaction"
                };
                say(progress, StepId::StopRunning, StepState::Waiting, hint);
            }
            sys.sleep(t.poll);
        }
        say(progress, StepId::StopRunning, StepState::Done, "The running copy closed");
    }

    // 3. Launch and verify.
    relaunch(sys, layout, t, progress)
}

/// Start the installed copy and wait until it owns the single-instance name.
pub fn relaunch(sys: &dyn System, layout: &Layout, t: &Timeouts, progress: &mut dyn FnMut(Progress)) -> EngineOutcome {
    say(progress, StepId::Launch, StepState::Running, "Starting the new version");
    let app = layout.app_path();
    let child = match sys.spawn_detached(&app) {
        Ok(pid) => pid,
        Err(reason) => {
            say(progress, StepId::Launch, StepState::Failed, reason.clone());
            return EngineOutcome::LaunchFailed { reason };
        }
    };
    let started = sys.now();
    loop {
        if let Some(owner) = sys.instance_pid() {
            if sys.appimage_of(owner).is_some_and(|p| same_file(&p, &app)) {
                say(progress, StepId::Launch, StepState::Done, "The new version is running");
                return EngineOutcome::Started { pid: owner };
            }
        }
        if let Some(reason) = sys.child_failed(child) {
            say(progress, StepId::Launch, StepState::Failed, reason.clone());
            return EngineOutcome::LaunchFailed { reason };
        }
        if sys.now() - started >= t.launch_cap {
            let reason = format!("it did not start within {} s", t.launch_cap.as_secs());
            say(progress, StepId::Launch, StepState::Failed, reason.clone());
            return EngineOutcome::LaunchFailed { reason };
        }
        sys.sleep(t.poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::testing::{appimage, layout, root};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const OLD: u32 = 10;
    const NEW: u32 = 20;

    /// A scripted machine. Time is virtual: `sleep` advances it.
    struct Fake {
        base: Instant,
        s: Mutex<St>,
    }
    struct St {
        clock: Duration,
        posd: Vec<u32>,
        old_running: bool,
        /// After a quit request, the old copy exits this much later (`None`: never — legacy).
        old_quits_after: Option<Duration>,
        quit_at: Option<Duration>,
        /// After the spawn, the new copy owns the name this much later (`None`: never).
        new_owns_after: Option<Duration>,
        spawned_at: Option<Duration>,
        child_fails: Option<String>,
        new_appimage: PathBuf,
        quit_requests: u32,
    }

    impl Fake {
        fn new(app: PathBuf) -> Self {
            Self {
                base: Instant::now(),
                s: Mutex::new(St {
                    clock: Duration::ZERO,
                    posd: vec![],
                    old_running: false,
                    old_quits_after: Some(Duration::from_secs(2)),
                    quit_at: None,
                    new_owns_after: Some(Duration::from_secs(1)),
                    spawned_at: None,
                    child_fails: None,
                    new_appimage: app,
                    quit_requests: 0,
                }),
            }
        }
        fn with(self, f: impl FnOnce(&mut St)) -> Self {
            f(&mut self.s.lock().unwrap());
            self
        }
    }

    impl System for Fake {
        fn posd_pids(&self) -> Vec<u32> {
            self.s.lock().unwrap().posd.clone()
        }
        fn instance_pid(&self) -> Option<u32> {
            let s = self.s.lock().unwrap();
            let old_alive = s.old_running
                && !matches!((s.quit_at, s.old_quits_after), (Some(q), Some(a)) if s.clock >= q + a);
            if old_alive {
                return Some(OLD);
            }
            match (s.spawned_at, s.new_owns_after) {
                (Some(t), Some(a)) if s.clock >= t + a => Some(NEW),
                _ => None,
            }
        }
        fn request_quit(&self) -> Result<(), String> {
            let mut s = self.s.lock().unwrap();
            s.quit_requests += 1;
            let now = s.clock;
            s.quit_at.get_or_insert(now);
            Ok(())
        }
        fn spawn_detached(&self, _exe: &std::path::Path) -> Result<u32, String> {
            let mut s = self.s.lock().unwrap();
            s.spawned_at = Some(s.clock);
            Ok(NEW)
        }
        fn child_failed(&self, _pid: u32) -> Option<String> {
            self.s.lock().unwrap().child_fails.clone()
        }
        fn appimage_of(&self, pid: u32) -> Option<PathBuf> {
            (pid == NEW).then(|| self.s.lock().unwrap().new_appimage.clone())
        }
        fn sleep(&self, d: Duration) {
            self.s.lock().unwrap().clock += d;
        }
        fn now(&self) -> Instant {
            self.base + self.s.lock().unwrap().clock
        }
    }

    fn go(sys: &Fake, r: &std::path::Path, installed: &Installed) -> (EngineOutcome, Vec<Progress>) {
        let l = layout(r);
        let src = appimage(r, "9.9.9");
        let mut seen = Vec::new();
        let out = run_update(sys, &l, &src, installed, &Timeouts::default(), &mut |p| seen.push(p));
        (out, seen)
    }

    #[test]
    fn a_running_posd_stops_the_update_before_anything_changes() {
        let r = root();
        let sys = Fake::new(layout(&r).app_path()).with(|s| s.posd = vec![4242]);
        let (out, _) = go(&sys, &r, &Installed::None);
        assert_eq!(out, EngineOutcome::PosdRunning { pids: vec![4242] });
        assert!(!layout(&r).app_path().exists(), "nothing was installed");
    }

    #[test]
    fn with_nothing_running_it_installs_and_launches() {
        let r = root();
        let sys = Fake::new(layout(&r).app_path());
        let (out, _) = go(&sys, &r, &Installed::None);
        assert_eq!(out, EngineOutcome::Started { pid: NEW });
        assert_eq!(std::fs::read_to_string(layout(&r).app_path()).unwrap(), "#appimage 9.9.9");
    }

    #[test]
    fn a_running_copy_is_asked_to_drain_and_the_new_one_starts_after_it() {
        let r = root();
        let sys = Fake::new(layout(&r).app_path()).with(|s| s.old_running = true);
        let (out, _) = go(&sys, &r, &Installed::Version(semver::Version::new(1, 0, 0)));
        assert_eq!(out, EngineOutcome::Started { pid: NEW });
        assert_eq!(sys.s.lock().unwrap().quit_requests, 1);
    }

    #[test]
    fn a_legacy_copy_gets_the_tray_hint_and_then_the_cap() {
        let r = root();
        let sys = Fake::new(layout(&r).app_path()).with(|s| {
            s.old_running = true;
            s.old_quits_after = None;
        });
        let (out, seen) = go(&sys, &r, &Installed::Legacy);
        assert_eq!(out, EngineOutcome::RunningNotStopped);
        assert!(
            seen.iter().any(|p| p.step == StepId::StopRunning && p.state == StepState::Waiting && p.message.contains("tray")),
            "{seen:?}"
        );
        // Files replaced; the old copy runs until restarted. (`install()` stamps the crate's
        // own version into the desktop entry, so check the bytes, not the key.)
        assert_eq!(std::fs::read_to_string(layout(&r).app_path()).unwrap(), "#appimage 9.9.9");
    }

    #[test]
    fn a_child_that_exits_is_a_launch_failure() {
        let r = root();
        let sys = Fake::new(layout(&r).app_path()).with(|s| {
            s.new_owns_after = None;
            s.child_fails = Some("exited with status 1".into());
        });
        assert!(matches!(go(&sys, &r, &Installed::None).0, EngineOutcome::LaunchFailed { .. }));
    }

    #[test]
    fn an_owner_that_is_not_the_installed_copy_is_not_success() {
        let r = root();
        let sys = Fake::new(PathBuf::from("/somewhere/else.AppImage"));
        assert!(matches!(go(&sys, &r, &Installed::None).0, EngineOutcome::LaunchFailed { .. }));
    }
}

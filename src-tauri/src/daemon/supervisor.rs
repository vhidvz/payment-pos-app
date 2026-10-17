//! The console's side of the service: reconcile at start, then keep the API answering for
//! as long as the console runs — without a `systemctl` call every tick while all is well.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use super::real::{ensure_with, RealHost};
use super::{Health, Host, Mode, Status};

/// How often the console looks at the API while it runs.
const TICK: Duration = Duration::from_secs(10);

/// The wait before the next look: a tick while all is well, doubling (to 5 min) for every
/// reconcile in a row that did not get the API serving — never a restart every tick.
pub fn next_wait(failures: u32) -> Duration {
    TICK.saturating_mul(1u32 << failures.min(5)).min(Duration::from_secs(300))
}

/// Whether the last reconcile still holds: the same `posd` answers that it left serving.
pub fn needs_ensure(last: &Status, now: &Option<Health>) -> bool {
    let answered = matches!(now, Some(h) if h.process.as_deref() == Some("posd") && last.version.as_deref() == Some(h.version.as_str()));
    !(last.state == "serving" && answered)
}

pub struct Supervisor {
    host: RealHost,
    status: RwLock<Status>,
    /// The installed copy is gone (uninstalled, or rolled back to a version without a
    /// service): run `posd` as our child for the rest of this session, set up nothing.
    ephemeral: AtomicBool,
    wake: (Mutex<bool>, Condvar),
}

impl Supervisor {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            host: RealHost::new(),
            status: RwLock::new(Status {
                mode: super::real::default_mode().label(),
                state: "starting",
                version: None,
                detail: None,
                spawned: None,
            }),
            ephemeral: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
        })
    }

    pub fn status(&self) -> Status {
        self.status.read().expect("daemon status").clone()
    }

    /// Reconcile now, then on every tick that finds the API wrong, until the process exits.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::Builder::new()
            .name("posd-supervisor".into())
            .spawn(move || {
              let mut failures = 0u32;
              loop {
                let last = me.status();
                if needs_ensure(&last, &me.host.health()) {
                    let next = ensure_with(&me.host, me.ephemeral.load(Ordering::SeqCst));
                    if next != last {
                        tracing::info!("API service: {} {}{}", next.mode, next.state, next.detail.as_deref().map(|d| format!(" — {d}")).unwrap_or_default());
                    }
                    // A child that this reconcile did not start is the one an earlier one did.
                    let spawned = next.spawned.or(last.spawned.filter(|_| next.mode == Mode::Child.label()));
                    failures = if next.state == "serving" { 0 } else { failures + 1 };
                    *me.status.write().expect("daemon status") = Status { spawned, ..next };
                }
                let (lock, cvar) = &me.wake;
                let woken = lock.lock().expect("wake");
                let (mut woken, timed_out) = cvar.wait_timeout_while(woken, next_wait(failures), |w| !*w).expect("wake");
                if !timed_out.timed_out() {
                    failures = 0; // asked for (an install): try at once, from the start
                }
                *woken = false;
              }
            })
            .expect("spawn the API supervisor");
    }

    /// Reconcile at the next moment rather than the next tick (after an install).
    pub fn wake(&self) {
        let mut status = self.status.write().expect("daemon status");
        status.state = "starting";
        drop(status);
        let (lock, cvar) = &self.wake;
        *lock.lock().expect("wake") = true;
        cvar.notify_all();
    }

    pub fn set_ephemeral(&self, on: bool) {
        self.ephemeral.store(on, Ordering::SeqCst);
    }

    /// Installed again after an uninstall: back to a real service.
    pub fn restore(&self) {
        self.set_ephemeral(false);
        self.wake();
    }

    /// The service is gone for good (uninstall, a rollback to before 1.2): undo it — `exe` is
    /// the `posd` it ran, named before an uninstall deletes it — and keep the API up only
    /// while this console runs. Blocks while that `posd` drains.
    pub fn go_ephemeral(&self, exe: &std::path::Path) {
        self.set_ephemeral(true);
        super::real::remove_with(&self.host, exe);
        self.wake();
    }

    /// The console is exiting: a child `posd` goes with it; a service or a detached one stays.
    pub fn shutdown(&self) {
        let status = self.status();
        if status.mode == Mode::Child.label() {
            if let Some(pid) = status.spawned {
                self.host.terminate(pid);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serving(version: &str) -> Status {
        Status { mode: "service", state: "serving", version: Some(version.into()), detail: None, spawned: None }
    }

    fn posd(version: &str) -> Option<Health> {
        Some(Health { version: version.into(), process: Some("posd".into()) })
    }

    #[test]
    fn reconciles_that_do_not_help_back_off() {
        assert_eq!(next_wait(0), Duration::from_secs(10));
        assert_eq!(next_wait(1), Duration::from_secs(20));
        assert_eq!(next_wait(3), Duration::from_secs(80));
        assert_eq!(next_wait(30), Duration::from_secs(300), "capped");
    }

    #[test]
    fn an_install_after_an_uninstall_is_permanent_again() {
        let s = Supervisor::new();
        s.set_ephemeral(true);
        s.restore();
        assert!(!s.ephemeral.load(Ordering::SeqCst));
    }

    #[test]
    fn a_healthy_service_is_left_alone() {
        assert!(!needs_ensure(&serving("1.2.0"), &posd("1.2.0")));
    }

    #[test]
    fn silence_a_new_version_or_a_stranger_calls_for_a_reconcile() {
        assert!(needs_ensure(&serving("1.2.0"), &None), "it crashed or was stopped");
        assert!(needs_ensure(&serving("1.2.0"), &posd("1.3.0")), "something else replaced it");
        assert!(needs_ensure(&serving("1.2.0"), &Some(Health { version: "1.2.0".into(), process: None })));
        let starting = Status { state: "starting", version: None, ..serving("1.2.0") };
        assert!(needs_ensure(&starting, &posd("1.2.0")), "confirm what it found once it answers");
    }
}

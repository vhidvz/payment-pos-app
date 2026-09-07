//! Administrator-owned security state.
//!
//! The password hash lives in a file only `root` can write, deliberately outside
//! `settings.json`: settings belong to the user running the app, so a hash kept
//! there could be deleted with a text editor. `posd admin set-password` writes
//! this file; the app only ever reads it.
//!
//! `autoLockMinutes` lives here too. It is a security control, and in the
//! user-writable settings file an operator could set it to 0 and defeat auto-lock.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use serde::{Deserialize, Serialize};

pub const DEFAULT_AUTO_LOCK_MINUTES: u32 = 15;
pub const MIN_PASSWORD_LEN: usize = 4;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AdminConfig {
    /// Argon2id PHC string. `None` means no password is set and the app is open.
    pub password_hash: Option<String>,
    /// Idle minutes before an unlocked session re-locks. 0 disables expiry.
    pub auto_lock_minutes: u32,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self { password_hash: None, auto_lock_minutes: DEFAULT_AUTO_LOCK_MINUTES }
    }
}

/// Where the administrator file lives on this platform.
///
/// Deliberately not overridable by an environment variable: pointing the app at
/// an empty file elsewhere would be a one-line bypass for exactly the user this
/// is meant to constrain.
pub fn admin_path() -> PathBuf {
    default_admin_path()
}

#[cfg(target_os = "windows")]
fn default_admin_path() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".to_string());
    PathBuf::from(base).join("ledger-pos").join("admin.json")
}

#[cfg(target_os = "macos")]
fn default_admin_path() -> PathBuf {
    PathBuf::from("/Library/Application Support/ledger-pos/admin.json")
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn default_admin_path() -> PathBuf {
    PathBuf::from("/etc/ledger-pos/admin.json")
}

/// Read the administrator file. A missing or unreadable file means "no password
/// set" — the app must stay usable, not refuse to start, when it is absent.
pub fn load_from(path: &Path) -> AdminConfig {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return AdminConfig::default();
    };
    serde_json::from_str(&raw).unwrap_or_else(|e| {
        tracing::warn!(
            "administrator file {} is unreadable ({e}); treating it as no password set",
            path.display()
        );
        AdminConfig::default()
    })
}

pub fn save_to(path: &Path, cfg: &AdminConfig) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(cfg).expect("admin config serialize");
    std::fs::write(path, json)?;
    // The app runs unprivileged and must be able to read this to verify a
    // password; only root may write it. An administrator running the daemon under
    // its own account can narrow this further (see `posd admin status`).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

/// Hash a password for storage. Returns a PHC string.
pub fn hash_password(password: &str) -> Result<String, String> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        ));
    }
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// Verify against a stored PHC string. A hash we cannot parse verifies nothing.
pub fn verify_password(password: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

// ------------------------------------------------------------------ sessions

/// One unlocked session. The password hash it was minted against is kept so a
/// password changed by the administrator — in a *different process* — invalidates
/// outstanding tokens without needing to signal the running daemon.
#[derive(Debug, Clone)]
struct Session {
    last_seen: Instant,
    hash: String,
}

/// Valid while it still matches the live password and has been used inside the
/// idle window. `window: None` means auto-lock is switched off.
fn session_valid(
    session: &Session,
    current_hash: &str,
    window: Option<Duration>,
    now: Instant,
) -> bool {
    if session.hash != current_hash {
        return false;
    }
    match window {
        None => true,
        Some(w) => now.duration_since(session.last_seen) <= w,
    }
}

/// Back-off after repeated wrong passwords: nothing for the first few, then
/// doubling seconds up to half a minute.
fn failure_delay(consecutive_failures: u32) -> Duration {
    if consecutive_failures < 3 {
        return Duration::ZERO;
    }
    let secs = 1u64
        .checked_shl(consecutive_failures - 3)
        .unwrap_or(u64::MAX)
        .min(30);
    Duration::from_secs(secs)
}

#[derive(Debug, PartialEq)]
pub enum UnlockError {
    NoPasswordSet,
    InvalidPassword,
}

/// Runtime lock state: the administrator file plus the sessions minted from it.
pub struct AuthState {
    admin_file: PathBuf,
    sessions: Mutex<HashMap<String, Session>>,
    failures: Mutex<u32>,
}

impl AuthState {
    pub fn new(admin_file: PathBuf) -> Self {
        Self {
            admin_file,
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new(0),
        }
    }

    /// Re-read on every use so `sudo posd admin set-password` takes effect without
    /// restarting the daemon. The file is a few dozen bytes and auth is rare.
    pub fn admin(&self) -> AdminConfig {
        load_from(&self.admin_file)
    }

    pub fn password_set(&self) -> bool {
        self.admin().password_hash.is_some()
    }

    /// Exchange a password for a session token.
    pub async fn unlock(&self, password: &str) -> Result<String, UnlockError> {
        let admin = self.admin();
        let Some(hash) = admin.password_hash else {
            return Err(UnlockError::NoPasswordSet);
        };

        // Read the counter and release the lock before awaiting.
        let delay = failure_delay(*self.failures.lock().expect("failure counter"));
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }

        if !verify_password(password, &hash) {
            let mut failures = self.failures.lock().expect("failure counter");
            *failures = failures.saturating_add(1);
            return Err(UnlockError::InvalidPassword);
        }
        *self.failures.lock().expect("failure counter") = 0;

        let token = uuid::Uuid::new_v4().to_string();
        self.sessions.lock().expect("sessions").insert(
            token.clone(),
            Session { last_seen: Instant::now(), hash },
        );
        Ok(token)
    }

    /// Drop one session.
    pub fn lock(&self, token: &str) {
        self.sessions.lock().expect("sessions").remove(token);
    }

    /// True when the caller may perform a protected action. Refreshes the
    /// session's idle clock as a side effect.
    pub fn is_unlocked(&self, token: Option<&str>) -> bool {
        let admin = self.admin();
        let Some(current) = admin.password_hash.as_deref() else {
            return true; // no password configured: the app is open, as before
        };
        let Some(token) = token else {
            return false;
        };
        let window = (admin.auto_lock_minutes > 0)
            .then(|| Duration::from_secs(u64::from(admin.auto_lock_minutes) * 60));
        let now = Instant::now();

        let mut sessions = self.sessions.lock().expect("sessions");
        let valid = match sessions.get_mut(token) {
            Some(s) if session_valid(s, current, window, now) => {
                s.last_seen = now;
                true
            }
            _ => false,
        };
        if !valid {
            // Expired or minted against a password that has since changed.
            sessions.remove(token);
        }
        valid
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    fn admin_file() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ledger-pos-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("admin.json")
    }

    fn with_password(pw: &str, auto_lock_minutes: u32) -> AuthState {
        let p = admin_file();
        save_to(
            &p,
            &AdminConfig {
                password_hash: Some(hash_password(pw).unwrap()),
                auto_lock_minutes,
            },
        )
        .unwrap();
        AuthState::new(p)
    }

    // -------------------------------------------------------- pure predicates

    #[test]
    fn a_session_within_the_idle_window_is_valid() {
        let s = Session { last_seen: Instant::now(), hash: "h".into() };
        assert!(session_valid(&s, "h", Some(Duration::from_secs(900)), Instant::now()));
    }

    #[test]
    fn a_session_idle_past_the_window_is_not() {
        let now = Instant::now();
        let s = Session { last_seen: now - Duration::from_secs(20 * 60), hash: "h".into() };
        assert!(!session_valid(&s, "h", Some(Duration::from_secs(15 * 60)), now));
    }

    /// autoLockMinutes = 0 means "never lock on idle".
    #[test]
    fn a_disabled_auto_lock_never_expires_a_session() {
        let now = Instant::now();
        let s = Session { last_seen: now - Duration::from_secs(30 * 24 * 3600), hash: "h".into() };
        assert!(session_valid(&s, "h", None, now));
    }

    /// The administrator changes the password in another process; sessions minted
    /// against the old one must stop working.
    #[test]
    fn a_session_minted_against_another_password_is_not_valid() {
        let s = Session { last_seen: Instant::now(), hash: "old".into() };
        assert!(!session_valid(&s, "new", None, Instant::now()));
    }

    #[test]
    fn back_off_starts_only_after_three_failures_and_is_capped() {
        assert_eq!(failure_delay(0), Duration::ZERO);
        assert_eq!(failure_delay(2), Duration::ZERO);
        assert_eq!(failure_delay(3), Duration::from_secs(1));
        assert_eq!(failure_delay(4), Duration::from_secs(2));
        assert_eq!(failure_delay(5), Duration::from_secs(4));
        assert_eq!(failure_delay(40), Duration::from_secs(30), "must stay bounded");
    }

    // ------------------------------------------------------------- AuthState

    /// With no password configured the app behaves exactly as it always has.
    #[test]
    fn everything_is_unlocked_when_no_password_is_set() {
        let state = AuthState::new(admin_file());
        assert!(!state.password_set());
        assert!(state.is_unlocked(None));
    }

    #[test]
    fn a_password_locks_callers_that_present_nothing() {
        let state = with_password("open sesame", 15);
        assert!(state.password_set());
        assert!(!state.is_unlocked(None));
    }

    #[tokio::test]
    async fn a_token_from_unlock_unlocks() {
        let state = with_password("open sesame", 15);
        let token = state.unlock("open sesame").await.expect("correct password");
        assert!(state.is_unlocked(Some(&token)));
    }

    #[tokio::test]
    async fn the_wrong_password_yields_no_token() {
        let state = with_password("open sesame", 15);
        assert_eq!(state.unlock("guess").await.unwrap_err(), UnlockError::InvalidPassword);
    }

    #[tokio::test]
    async fn unlocking_is_pointless_when_no_password_is_set() {
        let state = AuthState::new(admin_file());
        assert_eq!(state.unlock("whatever").await.unwrap_err(), UnlockError::NoPasswordSet);
    }

    #[test]
    fn an_unknown_token_does_not_unlock() {
        let state = with_password("open sesame", 15);
        assert!(!state.is_unlocked(Some("made-up")));
    }

    #[tokio::test]
    async fn locking_revokes_that_token() {
        let state = with_password("open sesame", 15);
        let token = state.unlock("open sesame").await.unwrap();
        state.lock(&token);
        assert!(!state.is_unlocked(Some(&token)));
    }

    /// End to end across processes: the CLI rewrites the file, the running
    /// daemon's tokens stop working.
    #[tokio::test]
    async fn changing_the_password_revokes_outstanding_tokens() {
        let state = with_password("first", 15);
        let token = state.unlock("first").await.unwrap();
        assert!(state.is_unlocked(Some(&token)));

        save_to(
            &state.admin_file,
            &AdminConfig {
                password_hash: Some(hash_password("second").unwrap()),
                auto_lock_minutes: 15,
            },
        )
        .unwrap();

        assert!(!state.is_unlocked(Some(&token)));
    }

    /// Removing the password opens the app; a stale token must not be an error.
    #[tokio::test]
    async fn clearing_the_password_opens_everything() {
        let state = with_password("first", 15);
        let token = state.unlock("first").await.unwrap();
        save_to(&state.admin_file, &AdminConfig::default()).unwrap();

        assert!(state.is_unlocked(None));
        assert!(state.is_unlocked(Some(&token)));
    }
}

// ------------------------------------------------------- administrator CLI

/// What `posd admin …` was asked to do.
#[derive(Debug, PartialEq)]
pub enum AdminCommand {
    SetPassword,
    ClearPassword,
    AutoLock(u32),
    Status,
}

/// Parse the arguments after the binary name. `None` means "not an admin
/// invocation" — the daemon should just start.
pub fn parse_admin_command(args: &[String]) -> Option<Result<AdminCommand, String>> {
    if args.first().map(String::as_str) != Some("admin") {
        return None;
    }
    const USAGE: &str = "usage: posd admin <set-password|clear-password|auto-lock <minutes>|status>";
    Some(match args.get(1).map(String::as_str) {
        Some("set-password") => Ok(AdminCommand::SetPassword),
        Some("clear-password") => Ok(AdminCommand::ClearPassword),
        Some("status") => Ok(AdminCommand::Status),
        Some("auto-lock") => match args.get(2) {
            Some(v) => v
                .parse::<u32>()
                .map(AdminCommand::AutoLock)
                .map_err(|_| format!("'{v}' is not a whole number of minutes\n{USAGE}")),
            None => Err(format!("auto-lock needs a number of minutes (0 disables it)\n{USAGE}")),
        },
        Some(other) => Err(format!("unknown admin command '{other}'\n{USAGE}")),
        None => Err(USAGE.to_string()),
    })
}

/// Read-modify-write of the administrator file, so one setting never clobbers another.
fn amend(path: &Path, change: impl FnOnce(&mut AdminConfig)) -> Result<(), String> {
    let mut cfg = load_from(path);
    change(&mut cfg);
    save_to(path, &cfg).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Store a new password, leaving the rest of the administrator file intact.
pub fn set_password_at(path: &Path, password: &str) -> Result<(), String> {
    // Hash first: a rejected password must not leave a half-written file behind.
    let hash = hash_password(password)?;
    amend(path, |cfg| cfg.password_hash = Some(hash))
}

/// Remove the password. The forgotten-password path.
pub fn clear_password_at(path: &Path) -> Result<(), String> {
    amend(path, |cfg| cfg.password_hash = None)
}

pub fn set_auto_lock_at(path: &Path, minutes: u32) -> Result<(), String> {
    amend(path, |cfg| cfg.auto_lock_minutes = minutes)
}

/// Human-readable summary of the administrator file.
pub fn status_report(path: &Path) -> String {
    let cfg = load_from(path);
    let mut out = format!("administrator file : {}\n", path.display());
    out.push_str(if cfg.password_hash.is_some() {
        "application        : a password is set\n"
    } else {
        "application        : open — no password is set\n"
    });
    out.push_str(&match cfg.auto_lock_minutes {
        0 => "auto-lock          : disabled\n".to_string(),
        m => format!("auto-lock          : {m} minutes idle\n"),
    });

    #[cfg(unix)]
    if cfg.password_hash.is_some() {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode() & 0o777;
            out.push_str(&format!("permissions        : {mode:04o}\n"));
            if mode & 0o004 != 0 {
                out.push_str(
                    "\nThe hash is readable by every user on this machine, which allows an\n\
                     offline guessing attack. If the daemon runs under its own account, narrow it:\n\
                     \n    sudo chgrp <daemon-user> <file> && sudo chmod 640 <file>\n",
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod admin_cli_tests {
    use super::*;

    fn tmp_admin() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ledger-pos-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("admin.json")
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plain_invocation_is_not_an_admin_command() {
        assert!(parse_admin_command(&args(&[])).is_none());
        assert!(parse_admin_command(&args(&["--background"])).is_none());
    }

    #[test]
    fn admin_subcommands_parse() {
        assert_eq!(parse_admin_command(&args(&["admin", "set-password"])), Some(Ok(AdminCommand::SetPassword)));
        assert_eq!(parse_admin_command(&args(&["admin", "clear-password"])), Some(Ok(AdminCommand::ClearPassword)));
        assert_eq!(parse_admin_command(&args(&["admin", "status"])), Some(Ok(AdminCommand::Status)));
        assert_eq!(parse_admin_command(&args(&["admin", "auto-lock", "30"])), Some(Ok(AdminCommand::AutoLock(30))));
    }

    /// A typo must not silently start the daemon instead.
    #[test]
    fn an_unknown_admin_subcommand_is_an_error_not_a_daemon_start() {
        let out = parse_admin_command(&args(&["admin", "set-pasword"]));
        assert!(matches!(out, Some(Err(_))), "got {out:?}");
        assert!(matches!(parse_admin_command(&args(&["admin"])), Some(Err(_))));
        assert!(matches!(parse_admin_command(&args(&["admin", "auto-lock"])), Some(Err(_))));
        assert!(matches!(parse_admin_command(&args(&["admin", "auto-lock", "soon"])), Some(Err(_))));
    }

    #[test]
    fn setting_a_password_leaves_the_other_settings_alone() {
        let p = tmp_admin();
        set_auto_lock_at(&p, 45).unwrap();
        set_password_at(&p, "open sesame").unwrap();

        let cfg = load_from(&p);
        assert_eq!(cfg.auto_lock_minutes, 45, "auto-lock must survive a password change");
        assert!(verify_password("open sesame", cfg.password_hash.as_deref().unwrap()));
    }

    #[test]
    fn clearing_removes_the_password_but_keeps_auto_lock() {
        let p = tmp_admin();
        set_auto_lock_at(&p, 45).unwrap();
        set_password_at(&p, "open sesame").unwrap();

        clear_password_at(&p).unwrap();

        let cfg = load_from(&p);
        assert_eq!(cfg.password_hash, None);
        assert_eq!(cfg.auto_lock_minutes, 45);
    }

    /// Clearing when nothing was ever set is a no-op, not a failure — that is
    /// exactly the state a confused administrator may be in.
    #[test]
    fn clearing_an_absent_file_succeeds() {
        assert!(clear_password_at(&tmp_admin()).is_ok());
    }

    #[test]
    fn a_short_password_is_refused_before_anything_is_written() {
        let p = tmp_admin();
        assert!(set_password_at(&p, "abc").is_err());
        assert!(!p.exists(), "nothing should have been written");
    }

    #[test]
    fn status_says_whether_a_password_is_set() {
        let p = tmp_admin();
        assert!(status_report(&p).contains("no password"));

        set_password_at(&p, "open sesame").unwrap();
        let out = status_report(&p);
        assert!(out.contains("password is set"), "got: {out}");
        assert!(!out.contains("$argon2"), "the report must not echo the hash: {out}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ledger-pos-sec-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("admin.json")
    }

    #[test]
    fn a_missing_admin_file_means_no_password_is_set() {
        let cfg = load_from(&tmp());
        assert_eq!(cfg.password_hash, None);
        assert_eq!(cfg.auto_lock_minutes, DEFAULT_AUTO_LOCK_MINUTES);
    }

    /// A corrupt file must not lock the owner out of their own terminal, nor panic
    /// the daemon on startup.
    #[test]
    fn a_malformed_admin_file_is_ignored_rather_than_fatal() {
        let p = tmp();
        std::fs::write(&p, b"{ this is not json").unwrap();
        assert_eq!(load_from(&p), AdminConfig::default());
    }

    #[test]
    fn a_saved_password_verifies_and_a_wrong_one_does_not() {
        let p = tmp();
        let cfg = AdminConfig {
            password_hash: Some(hash_password("correct horse").unwrap()),
            auto_lock_minutes: 5,
        };
        save_to(&p, &cfg).unwrap();

        let read_back = load_from(&p);
        assert_eq!(read_back.auto_lock_minutes, 5);
        let phc = read_back.password_hash.expect("hash should round trip");
        assert!(verify_password("correct horse", &phc));
        assert!(!verify_password("wrong horse", &phc));
    }

    /// The file must never contain anything a reader could use directly.
    #[test]
    fn the_stored_value_is_an_argon2_hash_not_the_password() {
        let phc = hash_password("hunter2").unwrap();
        assert!(phc.starts_with("$argon2"), "expected a PHC string, got {phc}");
        assert!(!phc.contains("hunter2"));
    }

    /// Two hashes of the same password differ, so the file cannot be used to tell
    /// whether two machines share a password.
    #[test]
    fn hashing_is_salted() {
        assert_ne!(hash_password("same").unwrap(), hash_password("same").unwrap());
    }

    #[test]
    fn a_garbage_hash_verifies_nothing() {
        assert!(!verify_password("anything", "not-a-phc-string"));
    }

    #[test]
    fn short_passwords_are_rejected() {
        assert!(hash_password("abc").is_err(), "below the {MIN_PASSWORD_LEN} char minimum");
        assert!(hash_password("abcd").is_ok());
    }
}

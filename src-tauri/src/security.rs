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

use crate::events::{generate_secret, EventsConfig, Subscription};
use crate::receipts::ReceiptKey;

pub const DEFAULT_AUTO_LOCK_MINUTES: u32 = 15;
pub const MIN_PASSWORD_LEN: usize = 4;

pub const ADMIN_SCHEMA: u32 = 1;
/// 0 → 1: unchanged in shape; only the version is stamped, by the next `posd admin` write.
pub const ADMIN_STEPS: &[crate::migrate::Step] =
    &[crate::migrate::Step { to: 1, apply: crate::migrate::noop }];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AdminConfig {
    /// Argon2id PHC string. `None` means no password is set and the app is open.
    pub password_hash: Option<String>,
    /// Idle minutes before an unlocked session re-locks. 0 disables expiry.
    pub auto_lock_minutes: u32,
    /// Webhook subscriptions ([`crate::events`]). Here rather than in settings so an
    /// operator who cannot change the password cannot silence them either.
    pub events: EventsConfig,
    /// The key that signs receipts ([`crate::receipts`]); `None` leaves them unsigned.
    pub receipt_key: Option<ReceiptKey>,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            password_hash: None,
            auto_lock_minutes: DEFAULT_AUTO_LOCK_MINUTES,
            events: EventsConfig::default(),
            receipt_key: None,
        }
    }
}

/// Where the administrator file lives on this platform.
///
/// Deliberately not overridable by an environment variable: pointing the app at
/// an empty file elsewhere would be a one-line bypass for exactly the user this
/// is meant to constrain. The one exception is a build with the `e2e` feature, which
/// release builds never enable, so the end-to-end harness never reads the real file.
pub fn admin_path() -> PathBuf {
    #[cfg(feature = "e2e")]
    if let Some(p) = std::env::var_os("LEDGER_POS_ADMIN_PATH") {
        return PathBuf::from(p);
    }
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
    try_load_from(path).unwrap_or_else(|e| {
        tracing::warn!("{e}; treating it as no password set");
        AdminConfig::default()
    })
}

/// The administrator file, or why it could not be read. A missing file is not an
/// error — it is the default, an open app.
///
/// **The optional sections are read leniently.** A webhook subscription or a
/// receipt key is a nested record a person may edit by hand, and a strict read
/// would let one bad entry fail the whole file — which [`load_from`] turns into
/// "no password set". So a bad entry is dropped with a warning, and only a file
/// that is not JSON at all, or whose password section is malformed, is an error.
pub fn try_load_from(path: &Path) -> Result<AdminConfig, String> {
    read_admin(path).map(|r| r.cfg)
}

/// The administrator file and the schema it was written with.
pub struct AdminRead {
    pub cfg: AdminConfig,
    /// The schema the file declared (0 for a pre-1.0 file; current for a missing one).
    pub schema: u32,
    /// `Some(found)` when a newer version wrote it — the console must stay locked.
    pub newer: Option<u32>,
    /// `Some(why)` when `schemaVersion` is there but is not a version (a hand edit): the
    /// key is ignored for reading, as before 1.0, and the file is never written back.
    pub invalid_schema: Option<String>,
}

/// The administrator file, migrated in memory (spec §2.3). Never writes: the app runs
/// unprivileged, and the next `sudo posd admin` write persists the new schema.
pub fn read_admin(path: &Path) -> Result<AdminRead, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AdminRead { cfg: AdminConfig::default(), schema: ADMIN_SCHEMA, newer: None, invalid_schema: None })
        }
        Err(e) => return Err(format!("administrator file {} is unreadable ({e})", path.display())),
    };
    let unreadable = |e: String| format!("administrator file {} is unreadable ({e})", path.display());
    let mut doc: serde_json::Value = serde_json::from_str(&raw).map_err(|e| unreadable(e.to_string()))?;
    let (schema, newer, invalid_schema) = match crate::migrate::schema_of(&doc) {
        Ok(found) => match crate::migrate::upgrade(&mut doc, ADMIN_STEPS) {
            Ok(_) => (found, None, None),
            Err(crate::migrate::MigrateError::Newer { found, .. }) => (found, Some(found), None),
            Err(e) => return Err(unreadable(e.to_string())),
        },
        // Not a version: ignore the key for reading, as every version before 1.0 did, so
        // the password stays enforced. It is never read as 0 and never written back.
        Err(e) => (0, None, Some(e.to_string())),
    };
    crate::migrate::strip(&mut doc);
    let cfg = match parse_lenient(path, doc) {
        Ok(cfg) => cfg,
        // A newer file this version cannot parse still keeps the console locked: fail-closed
        // must not depend on later versions keeping the step contract.
        Err(_) if newer.is_some() => AdminConfig::default(),
        Err(e) => return Err(unreadable(e)),
    };
    Ok(AdminRead { cfg, schema, newer, invalid_schema })
}

/// See [`try_load_from`]: one bad webhook or key entry is dropped with a warning instead
/// of failing the whole file.
fn parse_lenient(path: &Path, mut doc: serde_json::Value) -> Result<AdminConfig, String> {
    let events = doc.as_object_mut().and_then(|o| o.remove("events"));
    let receipt_key = doc.as_object_mut().and_then(|o| o.remove("receiptKey"));
    let mut cfg: AdminConfig = serde_json::from_value(doc).map_err(|e| e.to_string())?;

    if let Some(events) = events {
        cfg.events.device_id = events.get("deviceId").and_then(|v| v.as_str()).map(String::from);
        for entry in events.get("subscriptions").and_then(|v| v.as_array()).into_iter().flatten() {
            match serde_json::from_value::<Subscription>(entry.clone()) {
                Ok(sub) => cfg.events.subscriptions.push(sub),
                Err(e) => tracing::warn!("ignoring a malformed event subscription in {} ({e})", path.display()),
            }
        }
    }
    if let Some(key) = receipt_key.filter(|v| !v.is_null()) {
        match serde_json::from_value::<ReceiptKey>(key) {
            Ok(key) => cfg.receipt_key = Some(key),
            Err(e) => tracing::warn!("ignoring a malformed receipt key in {} ({e}); receipts go unsigned", path.display()),
        }
    }
    Ok(cfg)
}

pub fn save_to(path: &Path, cfg: &AdminConfig) -> std::io::Result<()> {
    let mut doc = serde_json::to_value(cfg).expect("admin config serialize");
    crate::migrate::stamp(&mut doc, ADMIN_SCHEMA).expect("an object");
    let json = serde_json::to_vec_pretty(&doc).expect("admin config serialize");
    // Write-then-rename: the running app re-reads this file every few seconds, and a
    // half-written one would read as "no password set" for that moment.
    //
    // The app runs unprivileged and must be able to read this to verify a
    // password; only root may write it. An administrator running the daemon under
    // its own account can narrow this further (see `posd admin status`).
    //
    // **The existing mode and owner are kept; 0644 is only for a new file.** This
    // used to re-apply 0644 on every write, so an administrator who narrowed the
    // file to 0640 — as `status` advises, and as the webhook secrets and the receipt
    // key now need — had it widened again by the next `set-password`, silently. And the
    // temp file is created with that mode from the start: it holds the hash and the
    // secrets, so it must never exist under the default umask, even for a moment.
    let previous = std::fs::metadata(path).ok();
    crate::fsutil::atomic_write(path, &json, previous.as_ref(), 0o644)
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
    /// A newer version wrote the administrator file: fail closed (spec §2.3).
    AdminFileNewer(u32),
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

    /// Where this state reads the administrator file from.
    pub fn admin_file_path(&self) -> &Path {
        &self.admin_file
    }

    /// Re-read on every use so `sudo posd admin set-password` takes effect without
    /// restarting the daemon. The file is a few dozen bytes and auth is rare.
    pub fn admin(&self) -> AdminConfig {
        load_from(&self.admin_file)
    }

    /// [`Self::admin`] without the fallback — for a reader that must tell "the file
    /// says no password" apart from "the file could not be read just now".
    pub fn try_admin(&self) -> Result<AdminConfig, String> {
        try_load_from(&self.admin_file)
    }

    pub fn password_set(&self) -> bool {
        self.admin().password_hash.is_some()
    }

    /// The file with its schema; an unreadable one reads as "no password set", as before.
    fn admin_read(&self) -> AdminRead {
        read_admin(&self.admin_file).unwrap_or_else(|e| {
            tracing::warn!("{e}; treating it as no password set");
            AdminRead { cfg: AdminConfig::default(), schema: 0, newer: None, invalid_schema: None }
        })
    }

    /// `Some(schema)` when a newer version wrote the administrator file: the console
    /// stays locked, because a lenient read of a renamed field could look like "no
    /// password" (spec §2.3, fail-closed).
    pub fn admin_file_newer(&self) -> Option<u32> {
        self.admin_read().newer
    }

    /// Exchange a password for a session token.
    pub async fn unlock(&self, password: &str) -> Result<String, UnlockError> {
        let read = self.admin_read();
        if let Some(found) = read.newer {
            return Err(UnlockError::AdminFileNewer(found));
        }
        let admin = read.cfg;
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

    /// Drop one session. `true` when there was one to drop.
    pub fn lock(&self, token: &str) -> bool {
        self.sessions.lock().expect("sessions").remove(token).is_some()
    }

    /// Whether protected changes can be made right now by *someone*: no password
    /// is set, or at least one session is still valid. The device-level answer a
    /// webhook receiver or a receipt verifier needs — [`Self::is_unlocked`] answers
    /// it for one caller.
    pub fn any_unlocked(&self) -> bool {
        let read = self.admin_read();
        if read.newer.is_some() {
            return false;
        }
        let admin = read.cfg;
        let Some(current) = admin.password_hash.as_deref() else {
            return true;
        };
        let window = (admin.auto_lock_minutes > 0)
            .then(|| Duration::from_secs(u64::from(admin.auto_lock_minutes) * 60));
        let now = Instant::now();
        self.sessions
            .lock()
            .expect("sessions")
            .values()
            .any(|s| session_valid(s, current, window, now))
    }

    /// True when the caller may perform a protected action. Refreshes the
    /// session's idle clock as a side effect.
    pub fn is_unlocked(&self, token: Option<&str>) -> bool {
        let read = self.admin_read();
        if read.newer.is_some() {
            return false;
        }
        let admin = read.cfg;
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
                ..AdminConfig::default()
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
                ..AdminConfig::default()
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
    /// `events add <url> [--events a,b]`
    EventsAdd { url: String, events: Vec<String> },
    EventsList,
    EventsRemove(String),
    /// `events test [<id>]` — every subscription when no id is given.
    EventsTest(Option<String>),
    /// `receipt-key set [--kid <kid>]`
    ReceiptKeySet(Option<String>),
    ReceiptKeyClear,
}

/// Parse the arguments after the binary name. `None` means "not an admin
/// invocation" — the daemon should just start.
pub fn parse_admin_command(args: &[String]) -> Option<Result<AdminCommand, String>> {
    if args.first().map(String::as_str) != Some("admin") {
        return None;
    }
    const USAGE: &str = "usage: posd admin <set-password|clear-password|auto-lock <minutes>|status>\n\
       posd admin events <add <url> [--events <type,prefix.*,…>]|list|remove <id>|test [<id>]>\n\
       posd admin receipt-key <set [--kid <id>]|clear>";
    Some(match args.get(1).map(String::as_str) {
        Some("set-password") => Ok(AdminCommand::SetPassword),
        Some("clear-password") => Ok(AdminCommand::ClearPassword),
        Some("status") => Ok(AdminCommand::Status),
        Some("events") => parse_events(&args[2..]).map_err(|e| format!("{e}\n{USAGE}")),
        Some("receipt-key") => match (args.get(2).map(String::as_str), args.get(3).map(String::as_str), args.get(4)) {
            (Some("set"), None, None) => Ok(AdminCommand::ReceiptKeySet(None)),
            (Some("set"), Some("--kid"), Some(kid)) if !kid.trim().is_empty() => Ok(AdminCommand::ReceiptKeySet(Some(kid.trim().to_string()))),
            (Some("clear"), None, None) => Ok(AdminCommand::ReceiptKeyClear),
            _ => Err(format!("receipt-key takes 'set [--kid <id>]' or 'clear'\n{USAGE}")),
        },
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

fn parse_events(rest: &[String]) -> Result<AdminCommand, String> {
    match rest.first().map(String::as_str) {
        Some("add") => {
            let url = rest.get(1).ok_or("events add needs a URL")?.trim().to_string();
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(format!("'{url}' is not an http(s) URL"));
            }
            let events = match (rest.get(2).map(String::as_str), rest.get(3)) {
                (None, _) => vec!["*".to_string()],
                (Some("--events"), Some(list)) => {
                    let parsed: Vec<String> =
                        list.split(',').map(|e| e.trim().to_string()).filter(|e| !e.is_empty()).collect();
                    for e in &parsed {
                        let known = e == "*"
                            || crate::events::types::ALL.contains(&e.as_str())
                            || e.strip_suffix(".*").is_some_and(|p| {
                                crate::events::types::ALL.iter().any(|t| t.starts_with(&format!("{p}.")))
                            });
                        if !known {
                            return Err(format!(
                                "'{e}' matches no event type; known: {}",
                                crate::events::types::ALL.join(", ")
                            ));
                        }
                    }
                    if parsed.is_empty() {
                        return Err("--events needs at least one type".into());
                    }
                    parsed
                }
                _ => return Err("events add takes '<url> [--events <list>]'".into()),
            };
            Ok(AdminCommand::EventsAdd { url, events })
        }
        Some("list") if rest.len() == 1 => Ok(AdminCommand::EventsList),
        Some("remove") => rest
            .get(1)
            .map(|id| AdminCommand::EventsRemove(id.trim().to_string()))
            .ok_or_else(|| "events remove needs a subscription id".to_string()),
        Some("test") => Ok(AdminCommand::EventsTest(rest.get(1).map(|s| s.trim().to_string()))),
        _ => Err("events takes add, list, remove or test".into()),
    }
}

/// Mint the installation's device id the first time something needs it.
fn ensure_device_id(cfg: &mut AdminConfig) {
    if cfg.events.device_id.is_none() {
        cfg.events.device_id = Some(format!("dev_{}", uuid::Uuid::new_v4().simple()));
    }
}

/// Add a webhook subscription; answers it **with its secret**, which is shown once.
pub fn add_subscription_at(path: &Path, url: &str, events: Vec<String>) -> Result<Subscription, String> {
    let sub = Subscription {
        id: format!("sub_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
        url: url.to_string(),
        secret: generate_secret()?,
        events,
        enabled: true,
    };
    let added = sub.clone();
    amend(path, |cfg| {
        ensure_device_id(cfg);
        cfg.events.subscriptions.push(sub);
    })?;
    Ok(added)
}

pub fn remove_subscription_at(path: &Path, id: &str) -> Result<(), String> {
    if !load_from(path).events.subscriptions.iter().any(|s| s.id == id) {
        return Err(format!("no subscription '{id}' (see `posd admin events list`)"));
    }
    amend(path, |cfg| cfg.events.subscriptions.retain(|s| s.id != id))
}

/// Set — or rotate — the receipt key; answers it **with its secret**, shown once.
pub fn set_receipt_key_at(path: &Path, kid: Option<String>) -> Result<ReceiptKey, String> {
    let key = ReceiptKey::generate(kid)?;
    let set = key.clone();
    amend(path, |cfg| {
        ensure_device_id(cfg);
        cfg.receipt_key = Some(key);
    })?;
    Ok(set)
}

pub fn clear_receipt_key_at(path: &Path) -> Result<(), String> {
    amend(path, |cfg| cfg.receipt_key = None)
}

/// Read-modify-write of the administrator file, so one setting never clobbers another.
fn amend(path: &Path, change: impl FnOnce(&mut AdminConfig)) -> Result<(), String> {
    // Never write over a file that did not parse: `load_from` would hand back the
    // defaults, and writing them would erase the password hash for good.
    let read = read_admin(path).map_err(|e| format!("{e}; fix or remove it by hand — nothing was changed"))?;
    // Nor over one a newer version wrote: this posd would drop what it does not know.
    if let Some(found) = read.newer {
        return Err(format!(
            "{} was written by a newer Ledger POS (schema {found}; this posd understands {ADMIN_SCHEMA}) \
             — use that version's posd; nothing was changed",
            path.display()
        ));
    }
    if let Some(why) = &read.invalid_schema {
        return Err(format!(
            "{}: its schemaVersion is not a version ({why}); fix or remove that key by hand — nothing was changed",
            path.display()
        ));
    }
    let mut cfg = read.cfg;
    change(&mut cfg);
    // The first write in the new schema keeps the original, with its mode and owner —
    // it holds the password hash and the secrets.
    if read.schema < ADMIN_SCHEMA && path.exists() {
        let bak = crate::migrate::backup_path(path, read.schema);
        crate::fsutil::backup_once(path, &bak)
            .map_err(|e| format!("cannot back up {} ({e}); nothing was changed", path.display()))?;
    }
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
    let read = read_admin(path).unwrap_or(AdminRead {
        cfg: AdminConfig::default(),
        schema: 0,
        newer: None,
        invalid_schema: None,
    });
    let cfg = read.cfg;
    let mut out = format!("administrator file : {}\n", path.display());
    out.push_str(&match (read.newer, read.schema) {
        _ if read.invalid_schema.is_some() => format!(
            "schema             : invalid ({}) — this posd will not change the file\n",
            read.invalid_schema.as_deref().unwrap_or_default()
        ),
        (Some(n), _) => format!("schema             : {n} — written by a newer Ledger POS; this posd will not change it\n"),
        (None, s) if s < ADMIN_SCHEMA => format!("schema             : {s} — upgraded to {ADMIN_SCHEMA} by the next change\n"),
        (None, s) => format!("schema             : {s}\n"),
    });
    out.push_str(if cfg.password_hash.is_some() {
        "application        : a password is set\n"
    } else {
        "application        : open — no password is set\n"
    });
    out.push_str(&match cfg.auto_lock_minutes {
        0 => "auto-lock          : disabled\n".to_string(),
        m => format!("auto-lock          : {m} minutes idle\n"),
    });

    if let Some(device) = &cfg.events.device_id {
        out.push_str(&format!("device id          : {device}\n"));
    }
    out.push_str(&match cfg.events.subscriptions.len() {
        0 => "event webhooks     : none\n".to_string(),
        n => format!("event webhooks     : {n} (posd admin events list)\n"),
    });
    out.push_str(&match &cfg.receipt_key {
        Some(k) => format!("signed receipts    : on, key id {}\n", k.kid),
        None => "signed receipts    : off\n".to_string(),
    });

    let holds_secrets = !cfg.events.subscriptions.is_empty() || cfg.receipt_key.is_some();
    #[cfg(unix)]
    if cfg.password_hash.is_some() || holds_secrets {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode() & 0o777;
            out.push_str(&format!("permissions        : {mode:04o}\n"));
            if mode & 0o004 != 0 {
                out.push_str(if holds_secrets {
                    "\nThe webhook secrets and the receipt key are readable by every user on this\n\
                     machine, so anyone here could sign a message as this device. If the daemon\n\
                     runs under its own account, narrow it:\n\
                     \n    sudo chgrp <daemon-user> <file> && sudo chmod 640 <file>\n"
                } else {
                    "\nThe hash is readable by every user on this machine, which allows an\n\
                     offline guessing attack. If the daemon runs under its own account, narrow it:\n\
                     \n    sudo chgrp <daemon-user> <file> && sudo chmod 640 <file>\n"
                });
            }
        }
    }
    #[cfg(not(unix))]
    let _ = holds_secrets;
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
            ..AdminConfig::default()
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

#[cfg(test)]
mod events_admin_tests {
    use super::*;

    fn tmp_admin() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ledger-pos-ev-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("admin.json")
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn events_and_receipt_key_commands_parse() {
        assert_eq!(
            parse_admin_command(&args(&["admin", "events", "add", "https://h/x"])),
            Some(Ok(AdminCommand::EventsAdd { url: "https://h/x".into(), events: vec!["*".into()] }))
        );
        assert_eq!(
            parse_admin_command(&args(&["admin", "events", "add", "https://h/x", "--events", "security.*, settings.changed"])),
            Some(Ok(AdminCommand::EventsAdd {
                url: "https://h/x".into(),
                events: vec!["security.*".into(), "settings.changed".into()]
            }))
        );
        assert_eq!(parse_admin_command(&args(&["admin", "events", "list"])), Some(Ok(AdminCommand::EventsList)));
        assert_eq!(parse_admin_command(&args(&["admin", "events", "remove", "sub_1"])), Some(Ok(AdminCommand::EventsRemove("sub_1".into()))));
        assert_eq!(parse_admin_command(&args(&["admin", "events", "test"])), Some(Ok(AdminCommand::EventsTest(None))));
        assert_eq!(parse_admin_command(&args(&["admin", "receipt-key", "set"])), Some(Ok(AdminCommand::ReceiptKeySet(None))));
        assert_eq!(
            parse_admin_command(&args(&["admin", "receipt-key", "set", "--kid", "shop-7"])),
            Some(Ok(AdminCommand::ReceiptKeySet(Some("shop-7".into()))))
        );
        assert_eq!(parse_admin_command(&args(&["admin", "receipt-key", "clear"])), Some(Ok(AdminCommand::ReceiptKeyClear)));
    }

    #[test]
    fn a_typo_in_an_event_type_or_a_non_http_url_is_refused() {
        assert!(matches!(parse_admin_command(&args(&["admin", "events", "add", "https://h", "--events", "securty.*"])), Some(Err(_))));
        assert!(matches!(parse_admin_command(&args(&["admin", "events", "add", "ftp://h"])), Some(Err(_))));
        assert!(matches!(parse_admin_command(&args(&["admin", "events", "add"])), Some(Err(_))));
        assert!(matches!(parse_admin_command(&args(&["admin", "receipt-key", "set", "--kid"])), Some(Err(_))));
    }

    #[test]
    fn adding_and_removing_a_subscription_keeps_the_rest_of_the_file() {
        let p = tmp_admin();
        set_auto_lock_at(&p, 7).unwrap();
        let sub = add_subscription_at(&p, "https://h/x", vec!["*".into()]).unwrap();
        let cfg = load_from(&p);
        assert_eq!(cfg.auto_lock_minutes, 7);
        assert_eq!(cfg.events.subscriptions, vec![sub.clone()]);
        assert!(cfg.events.device_id.as_deref().is_some_and(|d| d.starts_with("dev_")));
        assert!(remove_subscription_at(&p, "nope").is_err());
        remove_subscription_at(&p, &sub.id).unwrap();
        assert!(load_from(&p).events.subscriptions.is_empty());
        assert_eq!(load_from(&p).events.device_id, cfg.events.device_id, "the device keeps its id");
    }

    #[test]
    fn a_receipt_key_is_set_rotated_and_cleared() {
        let p = tmp_admin();
        let a = set_receipt_key_at(&p, Some("k1".into())).unwrap();
        let b = set_receipt_key_at(&p, None).unwrap();
        assert_ne!(a.secret, b.secret);
        assert_eq!(load_from(&p).receipt_key, Some(b));
        clear_receipt_key_at(&p).unwrap();
        assert_eq!(load_from(&p).receipt_key, None);
    }

    /// `save_to` used to re-apply 0644 on every write, widening a file the
    /// administrator had narrowed — the secrets now kept here depend on it.
    #[cfg(unix)]
    #[test]
    fn saving_keeps_a_narrowed_mode() {
        use std::os::unix::fs::PermissionsExt;
        let p = tmp_admin();
        set_auto_lock_at(&p, 5).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o644, "created 0644");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
        set_auto_lock_at(&p, 6).unwrap();
        add_subscription_at(&p, "https://h/x", vec!["*".into()]).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o640, "narrowing survives writes");
    }

    #[test]
    fn an_old_admin_file_without_the_new_fields_still_loads() {
        let p = tmp_admin();
        std::fs::write(&p, r#"{"passwordHash":null,"autoLockMinutes":3}"#).unwrap();
        let cfg = load_from(&p);
        assert_eq!(cfg.auto_lock_minutes, 3);
        assert!(cfg.events.subscriptions.is_empty() && cfg.receipt_key.is_none());
    }

    #[test]
    fn status_names_events_and_receipts() {
        let p = tmp_admin();
        add_subscription_at(&p, "https://h/x", vec!["*".into()]).unwrap();
        set_receipt_key_at(&p, Some("k9".into())).unwrap();
        let report = status_report(&p);
        assert!(report.contains("event webhooks     : 1"));
        assert!(report.contains("key id k9"));
        assert!(report.contains("anyone here could sign a message"), "world-readable secrets are called out:\n{report}");
    }

    /// One bad hand edit of a webhook entry used to fail the whole file — which
    /// `load_from` reads as "no password" — and the next `posd admin` write made
    /// that permanent.
    #[test]
    fn a_malformed_subscription_is_dropped_and_the_password_survives() {
        let p = tmp_admin();
        std::fs::write(
            &p,
            r#"{"passwordHash":"$argon2id$x","autoLockMinutes":4,
                "events":{"deviceId":"dev_1","subscriptions":[{"id":"ok","url":"https://h","secret":"whsec_AAAA"},{"url":"broken"}]},
                "receiptKey":{"kid":"k"}}"#,
        )
        .unwrap();
        let cfg = try_load_from(&p).unwrap();
        assert_eq!(cfg.password_hash.as_deref(), Some("$argon2id$x"));
        assert_eq!(cfg.auto_lock_minutes, 4);
        assert_eq!(cfg.events.device_id.as_deref(), Some("dev_1"));
        assert_eq!(cfg.events.subscriptions.len(), 1, "the good entry stays, the broken one goes");
        assert!(cfg.receipt_key.is_none(), "a key without its secret signs nothing");
    }

    #[test]
    fn a_write_never_lands_on_a_file_that_did_not_parse() {
        let p = tmp_admin();
        std::fs::write(&p, "{ this is not json").unwrap();
        assert!(set_auto_lock_at(&p, 9).is_err());
        assert!(add_subscription_at(&p, "https://h", vec!["*".into()]).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ this is not json", "left exactly as it was");
        assert!(try_load_from(&p).is_err());
        assert_eq!(load_from(&p), AdminConfig::default(), "the app itself still starts, open");
    }

    #[test]
    fn a_missing_file_is_the_default_not_an_error() {
        let p = tmp_admin();
        assert_eq!(try_load_from(&p).unwrap(), AdminConfig::default());
    }
}


#[cfg(test)]
mod schema_tests {
    use super::*;

    fn admin_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ledger-admin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_admin_chain_is_contiguous() {
        crate::migrate::assert_contiguous(ADMIN_STEPS);
    }

    #[test]
    fn a_v0_file_reads_in_memory_and_is_not_written_by_reading() {
        let d = admin_dir();
        let p = d.join("admin.json");
        let v0 = format!(r#"{{"passwordHash":"{}","autoLockMinutes":7}}"#, hash_password("secret").unwrap());
        std::fs::write(&p, &v0).unwrap();
        let read = read_admin(&p).unwrap();
        assert_eq!((read.schema, read.newer, read.cfg.auto_lock_minutes), (0, None, 7));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), v0, "reading never writes");
    }

    #[cfg(unix)]
    #[test]
    fn the_first_write_backs_up_with_the_originals_mode_then_stamps() {
        use std::os::unix::fs::PermissionsExt;
        let d = admin_dir();
        let p = d.join("admin.json");
        std::fs::write(&p, r#"{"autoLockMinutes":7}"#).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();

        set_auto_lock_at(&p, 9).unwrap();

        let bak = d.join("admin.json.v0.bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), r#"{"autoLockMinutes":7}"#);
        assert_eq!(std::fs::metadata(&bak).unwrap().permissions().mode() & 0o777, 0o640);
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o640);
        let on_disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!((on_disk["schemaVersion"].as_u64(), on_disk["autoLockMinutes"].as_u64()), (Some(1), Some(9)));
    }

    #[tokio::test]
    async fn a_newer_admin_file_fails_closed() {
        let d = admin_dir();
        let p = d.join("admin.json");
        // A newer version that renamed the hash field: an older lenient read would see "no
        // password" — which must NOT open the console.
        std::fs::write(&p, r#"{"schemaVersion":5,"credential":{"hash":"x"}}"#).unwrap();
        let auth = AuthState::new(p.clone());
        assert_eq!(auth.admin_file_newer(), Some(5));
        assert!(!auth.is_unlocked(None));
        assert!(!auth.any_unlocked());
        assert_eq!(auth.unlock("anything").await, Err(UnlockError::AdminFileNewer(5)));
    }

    #[test]
    fn posd_refuses_to_write_a_newer_admin_file() {
        let d = admin_dir();
        let p = d.join("admin.json");
        let newer = r#"{"schemaVersion":5}"#;
        std::fs::write(&p, newer).unwrap();
        let err = set_auto_lock_at(&p, 3).unwrap_err();
        assert!(err.contains("newer"), "{err}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), newer);
    }

    #[test]
    fn status_reports_the_schema() {
        let d = admin_dir();
        let p = d.join("admin.json");
        std::fs::write(&p, r#"{"autoLockMinutes":7}"#).unwrap();
        assert!(status_report(&p).contains("schema             : 0 — upgraded to 1 by the next change"));
    }

    /// A newer file whose known fields no longer parse (a type changed) must still keep the
    /// console locked — fail-closed cannot depend on later versions keeping the contract.
    #[tokio::test]
    async fn a_newer_admin_file_that_does_not_parse_still_fails_closed() {
        let d = admin_dir();
        let p = d.join("admin.json");
        std::fs::write(&p, r#"{"schemaVersion":5,"autoLockMinutes":"soon","passwordHash":7}"#).unwrap();
        let auth = AuthState::new(p.clone());
        assert_eq!(auth.admin_file_newer(), Some(5));
        assert!(!auth.is_unlocked(None));
        assert!(!auth.any_unlocked());
        assert_eq!(auth.unlock("anything").await, Err(UnlockError::AdminFileNewer(5)));
    }

    /// A hand-edited schemaVersion that is not a version is ignored for reading, as every
    /// version before 1.0 ignored the key — the password stays enforced — and is never
    /// written back.
    #[tokio::test]
    async fn an_invalid_schema_version_keeps_the_password_enforced() {
        let d = admin_dir();
        let p = d.join("admin.json");
        let raw = format!(r#"{{"schemaVersion":"1","passwordHash":"{}"}}"#, hash_password("secret").unwrap());
        std::fs::write(&p, &raw).unwrap();
        let auth = AuthState::new(p.clone());
        assert!(auth.password_set());
        assert!(!auth.is_unlocked(None), "locked: the hash is still read");
        assert!(auth.unlock("secret").await.is_ok());
        let err = set_auto_lock_at(&p, 3).unwrap_err();
        assert!(err.contains("schemaVersion"), "{err}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), raw, "never written back");
        assert!(status_report(&p).contains("schema             : invalid"));
    }

    #[cfg(not(feature = "e2e"))]
    #[test]
    fn release_builds_ignore_the_admin_path_override() {
        std::env::set_var("LEDGER_POS_ADMIN_PATH", "/tmp/elsewhere.json");
        assert_eq!(admin_path(), default_admin_path());
        std::env::remove_var("LEDGER_POS_ADMIN_PATH");
    }
}

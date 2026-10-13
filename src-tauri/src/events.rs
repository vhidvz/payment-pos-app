//! Webhook events — optional, off until an administrator adds a subscription.
//!
//! Some changes to this machine matter to someone who is not sitting at it: the
//! lock was opened, the password removed, the terminal repointed, the browser
//! policy widened. With `sudo posd admin events add <url>` the app posts each such
//! change to that URL, in the [Standard Webhooks](https://www.standardwebhooks.com)
//! format (`webhook-id`, `webhook-timestamp`, `webhook-signature: v1,…`), so any
//! server verifies it with an off-the-shelf library and nothing here is specific
//! to whoever is listening.
//!
//! ## What a receiver can and cannot conclude
//!
//! Every event carries the device's [posture](crate::posture::Posture), including
//! the fingerprint of its security-relevant settings, so *any* event — not only
//! the one about a change — tells a receiver whether the device still matches what
//! it accepted. That matters because two changes cannot be seen live: a
//! `settings.json` edited by hand while the app is stopped (the file is read at
//! start-up, so it shows as a new fingerprint in `app.started`), and anything done
//! by an administrator of the machine, who can also remove this configuration.
//! **The absence of an event is not evidence that nothing changed** — a receiver
//! that needs that guarantee should rely on [signed receipts](crate::receipts),
//! which carry the same posture with every sale.
//!
//! ## Where the configuration lives
//!
//! In the administrator file beside the password hash, written only by
//! `sudo posd admin events …`: an operator who cannot change the password cannot
//! silence the events either. The running app re-reads that file every few
//! seconds, so a change takes effect without a restart — and a change *to the
//! subscriptions themselves* is announced (`events.config_changed`) to the old
//! endpoints as well as the new, so replacing a URL cannot happen quietly.
//!
//! Undelivered messages wait in `events-outbox.json` next to the settings and are
//! retried with backoff (5 s doubling to 15 min) for up to 72 hours.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use utoipa::ToSchema;

use crate::posture::{changed_paths, posture, security_digest};
use crate::security::AdminConfig;
use crate::server::AppState;
use crate::settings::Settings;

/// Event types. A subscription names them exactly, by prefix (`security.*`), or all (`*`).
pub mod types {
    pub const APP_STARTED: &str = "app.started";
    pub const APP_STOPPING: &str = "app.stopping";
    pub const SECURITY_UNLOCKED: &str = "security.unlocked";
    pub const SECURITY_LOCKED: &str = "security.locked";
    pub const SECURITY_PASSWORD_SET: &str = "security.password_set";
    pub const SECURITY_PASSWORD_CLEARED: &str = "security.password_cleared";
    pub const SECURITY_AUTO_LOCK_CHANGED: &str = "security.auto_lock_changed";
    pub const SETTINGS_CHANGED: &str = "settings.changed";
    pub const EVENTS_CONFIG_CHANGED: &str = "events.config_changed";
    pub const RECEIPTS_KEY_CHANGED: &str = "receipts.key_changed";
    pub const WEBHOOK_TEST: &str = "webhook.test";

    pub const ALL: &[&str] = &[
        APP_STARTED,
        APP_STOPPING,
        SECURITY_UNLOCKED,
        SECURITY_LOCKED,
        SECURITY_PASSWORD_SET,
        SECURITY_PASSWORD_CLEARED,
        SECURITY_AUTO_LOCK_CHANGED,
        SETTINGS_CHANGED,
        EVENTS_CONFIG_CHANGED,
        RECEIPTS_KEY_CHANGED,
        WEBHOOK_TEST,
    ];
}

/// How often the background task re-reads the administrator file and retries
/// deliveries. A local file read; it costs no receiver anything.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);
pub const MAX_AGE_SECS: i64 = 72 * 3600;
pub const MAX_BACKOFF_SECS: i64 = 15 * 60;
const RECENT_LIMIT: usize = 50;
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EventsConfig {
    /// A stable id for this installation, sent as `data.device.id` in every event
    /// and as `sub` in signed receipts. Minted by the first `events add` or
    /// `receipt-key set`.
    pub device_id: Option<String>,
    pub subscriptions: Vec<Subscription>,
}

fn all_events() -> Vec<String> {
    vec!["*".to_string()]
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    pub id: String,
    pub url: String,
    /// `whsec_` + base64, as Standard Webhooks writes it.
    pub secret: String,
    /// Event types, `prefix.*` patterns, or `*`.
    #[serde(default = "all_events")]
    pub events: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// Whether a subscription's patterns admit an event type.
pub fn matches(patterns: &[String], ty: &str) -> bool {
    patterns.iter().any(|p| {
        p == "*"
            || p == ty
            || p.strip_suffix(".*")
                .is_some_and(|prefix| ty.starts_with(&format!("{prefix}.")))
    })
}

/// A fresh signing secret in the Standard Webhooks form.
pub fn generate_secret() -> Result<String, String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| format!("no randomness available: {e}"))?;
    Ok(format!("whsec_{}", STANDARD.encode(bytes)))
}

/// The `webhook-signature` header value: `v1,` + base64(HMAC-SHA256(key, "{id}.{timestamp}.{body}")).
pub fn sign(secret: &str, msg_id: &str, timestamp: i64, body: &str) -> Result<String, String> {
    let raw = secret.strip_prefix("whsec_").unwrap_or(secret);
    let key = STANDARD
        .decode(raw)
        // Never echo the decoder's error: it quotes the offending byte of the secret.
        .map_err(|_| "the webhook secret is not valid base64".to_string())?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|e| e.to_string())?;
    mac.update(format!("{msg_id}.{timestamp}.{body}").as_bytes());
    Ok(format!(
        "v1,{}",
        STANDARD.encode(mac.finalize().into_bytes())
    ))
}

/// Seconds to wait after the `attempts`-th failure: 5, 10, 20 … capped at 15 minutes.
pub fn backoff_secs(attempts: u32) -> i64 {
    (5i64 << attempts.min(20)).min(MAX_BACKOFF_SECS)
}

/// One message waiting for one subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    /// `webhook-id` — the same on every retry, so a receiver can de-duplicate.
    pub msg_id: String,
    pub event_type: String,
    pub subscription: String,
    pub url: String,
    pub body: String,
    pub created_at: i64,
    pub attempts: u32,
    pub next_at: i64,
}

/// What became of a delivery attempt — what `GET /api/v1/events` lists.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryRecord {
    pub at: String,
    pub msg_id: String,
    pub event_type: String,
    pub subscription: String,
    /// `delivered`, `retrying`, `gone` (the receiver answered 410), `expired`, or `orphaned`
    /// (the subscription was removed before the message could be signed).
    pub outcome: String,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A subscription as the API shows it — never with its secret.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionView {
    pub id: String,
    pub url: String,
    pub events: Vec<String>,
    pub enabled: bool,
}

/// `scheme://host[:port]`, with `/…` when there was more: a webhook URL can carry a
/// token in its path (chat-service hooks do), and the view is served unauthenticated
/// to anything on this machine.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "…".to_string();
    };
    let (authority, tail) = rest.split_once('/').map_or((rest, ""), |(a, t)| (a, t));
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let authority = authority.split(['?', '#']).next().unwrap_or(authority);
    if tail.is_empty() && !rest.contains(['?', '#']) {
        format!("{scheme}://{authority}")
    } else {
        format!("{scheme}://{authority}/…")
    }
}

impl From<&Subscription> for SubscriptionView {
    fn from(s: &Subscription) -> Self {
        Self {
            id: s.id.clone(),
            url: redact_url(&s.url),
            events: s.events.clone(),
            enabled: s.enabled,
        }
    }
}

/// The event body: `{ "type", "timestamp", "data" }`, with `data.device` added to every one.
pub fn envelope(
    ty: &str,
    timestamp: &str,
    device_id: Option<&str>,
    device_posture: Value,
    mut data: Value,
) -> String {
    if !data.is_object() {
        data = json!({});
    }
    data["device"] = json!({ "id": device_id, "posture": device_posture });
    json!({ "type": ty, "timestamp": timestamp, "data": data }).to_string()
}

pub struct EventBus {
    outbox_path: Option<PathBuf>,
    outbox: Mutex<Vec<Delivery>>,
    /// Every subscription seen during this process's life, by id. A message queued
    /// for a subscription that has since been removed — `events.config_changed` goes
    /// to the endpoints being replaced — can still be signed with its secret.
    known: Mutex<HashMap<String, Subscription>>,
    recent: Mutex<VecDeque<DeliveryRecord>>,
    /// Held across snapshot, write and rename, so concurrent emits and deliveries
    /// cannot interleave two writes or rename an older snapshot over a newer one.
    persist_lock: Mutex<()>,
    client: reqwest::Client,
}

/// The HTTP client every delivery uses. Redirects are **not** followed: a `301`
/// would turn the signed `POST` into a body-less `GET` that answers `200`, and the
/// message would be recorded as delivered without ever arriving.
fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("LedgerPOS/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

/// Read the outbox file. An unreadable one is kept aside as `.invalid` rather than
/// silently replaced — undelivered messages are evidence somebody may want back.
fn load_outbox(path: &std::path::Path) -> Vec<Delivery> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<Delivery>>(&raw) {
        Ok(outbox) => outbox,
        Err(e) => {
            tracing::warn!(
                "event outbox {} is unreadable ({e}); keeping it as .invalid and starting empty",
                path.display()
            );
            let _ = std::fs::copy(path, path.with_extension("json.invalid"));
            Vec::new()
        }
    }
}

impl EventBus {
    /// `outbox_path: None` keeps undelivered messages in memory only (tests, one-shot CLI use).
    pub fn new(outbox_path: Option<PathBuf>) -> Self {
        let outbox = outbox_path.as_deref().map(load_outbox).unwrap_or_default();
        Self {
            outbox_path,
            outbox: Mutex::new(outbox),
            known: Mutex::new(HashMap::new()),
            recent: Mutex::new(VecDeque::new()),
            persist_lock: Mutex::new(()),
            client: http_client().expect("http client"),
        }
    }

    /// The outbox beside `settings.json`.
    pub fn default_outbox_path() -> PathBuf {
        crate::settings::settings_path().with_file_name("events-outbox.json")
    }

    pub fn remember(&self, subscriptions: &[Subscription]) {
        let mut known = self.known.lock().expect("known");
        for s in subscriptions {
            known.insert(s.id.clone(), s.clone());
        }
    }

    /// Queue `ty` for every enabled subscription that wants it, stamped with the
    /// device's posture now.
    pub fn emit(&self, state: &AppState, ty: &str, data: Value) {
        let admin = state.auth.admin();
        self.emit_to(state, &admin, &admin.events.subscriptions, ty, data);
    }

    fn emit_to(
        &self,
        state: &AppState,
        admin: &AdminConfig,
        subscriptions: &[Subscription],
        ty: &str,
        data: Value,
    ) {
        let wanted: Vec<&Subscription> = subscriptions
            .iter()
            .filter(|s| s.enabled && matches(&s.events, ty))
            .collect();
        if wanted.is_empty() {
            return;
        }
        let now = chrono::Utc::now();
        let device_posture = serde_json::to_value(posture(&state.auth, &state.settings.get()))
            .unwrap_or(Value::Null);
        let body = envelope(
            ty,
            &now.to_rfc3339(),
            admin.events.device_id.as_deref(),
            device_posture,
            data,
        );
        self.remember(subscriptions);
        {
            let mut outbox = self.outbox.lock().expect("outbox");
            for s in wanted {
                outbox.push(Delivery {
                    msg_id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
                    event_type: ty.to_string(),
                    subscription: s.id.clone(),
                    url: s.url.clone(),
                    body: body.clone(),
                    created_at: now.timestamp(),
                    attempts: 0,
                    next_at: now.timestamp(),
                });
            }
        }
        self.persist();
    }

    /// The waiting deliveries themselves — tests read their schedule.
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> Vec<Delivery> {
        self.outbox.lock().expect("outbox").clone()
    }

    /// The bodies waiting, oldest first — what a test or a diagnostic reads.
    pub fn queued_bodies(&self) -> Vec<String> {
        self.outbox
            .lock()
            .expect("outbox")
            .iter()
            .map(|d| d.body.clone())
            .collect()
    }

    pub fn pending(&self) -> usize {
        self.outbox.lock().expect("outbox").len()
    }

    pub fn recent(&self) -> Vec<DeliveryRecord> {
        self.recent
            .lock()
            .expect("recent")
            .iter()
            .cloned()
            .collect()
    }

    fn record(&self, d: &Delivery, outcome: &str, detail: Option<String>) {
        let mut recent = self.recent.lock().expect("recent");
        recent.push_front(DeliveryRecord {
            at: chrono::Utc::now().to_rfc3339(),
            msg_id: d.msg_id.clone(),
            event_type: d.event_type.clone(),
            subscription: d.subscription.clone(),
            outcome: outcome.to_string(),
            attempts: d.attempts,
            detail,
        });
        recent.truncate(RECENT_LIMIT);
    }

    fn persist(&self) {
        let Some(path) = &self.outbox_path else {
            return;
        };
        let _guard = self.persist_lock.lock().expect("persist lock");
        // Snapshot inside the guard: the last writer always writes the newest state.
        let snapshot = self.outbox.lock().expect("outbox").clone();
        let json = serde_json::to_string_pretty(&snapshot).expect("outbox serialize");
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, path)) {
            tracing::warn!("cannot persist the event outbox {}: {e}", path.display());
        }
    }

    /// Try every message that is due. Never holds a lock across a send.
    pub async fn deliver_due(&self) {
        self.deliver_where(|_| true).await;
    }

    /// Messages of one type still waiting — what a bounded shutdown waits on.
    pub fn pending_of(&self, event_type: &str) -> usize {
        self.outbox
            .lock()
            .expect("outbox")
            .iter()
            .filter(|d| d.event_type == event_type)
            .count()
    }

    async fn deliver_where(&self, wanted: impl Fn(&Delivery) -> bool) {
        let now = chrono::Utc::now().timestamp();
        let due: Vec<Delivery> = self
            .outbox
            .lock()
            .expect("outbox")
            .iter()
            .filter(|d| d.next_at <= now && wanted(d))
            .cloned()
            .collect();
        if due.is_empty() {
            return;
        }
        let mut finished: Vec<String> = Vec::new();
        let mut retry: Vec<(String, u32, i64)> = Vec::new();
        for d in due {
            if now - d.created_at > MAX_AGE_SECS {
                self.record(&d, "expired", None);
                finished.push(d.msg_id.clone());
                continue;
            }
            // The URL and the secret come from the subscription, never from the
            // outbox: that file is writable by the account the app runs as, and
            // trusting its `url` would let it point a validly signed message anywhere.
            let target = self
                .known
                .lock()
                .expect("known")
                .get(&d.subscription)
                .map(|s| (s.url.clone(), s.secret.clone()));
            let Some((url, secret)) = target else {
                self.record(&d, "orphaned", None);
                finished.push(d.msg_id.clone());
                continue;
            };
            match send_signed(&self.client, &url, &d.msg_id, &d.body, &secret).await {
                Ok(status) if (200..300).contains(&status) => {
                    let mut done = d.clone();
                    done.attempts += 1;
                    self.record(&done, "delivered", Some(status.to_string()));
                    finished.push(d.msg_id.clone());
                }
                Ok(410) => {
                    self.record(&d, "gone", Some("410".into()));
                    finished.push(d.msg_id.clone());
                }
                outcome => {
                    let attempts = d.attempts + 1;
                    let detail = match outcome {
                        Ok(status) => status.to_string(),
                        Err(e) => e,
                    };
                    let mut failed = d.clone();
                    failed.attempts = attempts;
                    self.record(&failed, "retrying", Some(detail));
                    // `attempts` counts this failure, so the first retry waits backoff_secs(0).
                    retry.push((d.msg_id.clone(), attempts, now + backoff_secs(attempts - 1)));
                }
            }
        }
        {
            let mut outbox = self.outbox.lock().expect("outbox");
            outbox.retain(|d| !finished.contains(&d.msg_id));
            for (id, attempts, next_at) in retry {
                if let Some(d) = outbox.iter_mut().find(|d| d.msg_id == id) {
                    d.attempts = attempts;
                    d.next_at = next_at;
                }
            }
        }
        self.persist();
    }
}

/// POST one signed message; the HTTP status, or why there was none.
pub async fn send_signed(
    client: &reqwest::Client,
    url: &str,
    msg_id: &str,
    body: &str,
    secret: &str,
) -> Result<u16, String> {
    let timestamp = chrono::Utc::now().timestamp();
    let signature = sign(secret, msg_id, timestamp, body)?;
    client
        .post(url)
        .header("content-type", "application/json")
        .header("webhook-id", msg_id)
        .header("webhook-timestamp", timestamp.to_string())
        .header("webhook-signature", signature)
        .body(body.to_string())
        .send()
        .await
        .map(|r| r.status().as_u16())
        // `without_url`: a webhook URL may carry a token in its path, and this text
        // ends up in the delivery log that `GET /api/v1/events` serves.
        .map_err(|e| e.without_url().to_string())
}

/// `webhook.test` to one subscription, sent at once — what `posd admin events test` runs.
pub async fn send_test(sub: &Subscription, device_id: Option<&str>) -> Result<u16, String> {
    let client = http_client()?;
    let body = envelope(
        types::WEBHOOK_TEST,
        &chrono::Utc::now().to_rfc3339(),
        device_id,
        Value::Null,
        json!({}),
    );
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    send_signed(&client, &sub.url, &msg_id, &body, &sub.secret).await
}

/// What changed in the administrator file between two reads, as events.
///
/// The third element is `true` for `events.config_changed`, which goes to the old
/// subscriptions as well as the new ones.
pub fn admin_changes(
    before: &AdminConfig,
    after: &AdminConfig,
) -> Vec<(&'static str, Value, bool)> {
    let mut out = Vec::new();
    match (&before.password_hash, &after.password_hash) {
        (None, Some(_)) => out.push((
            types::SECURITY_PASSWORD_SET,
            json!({ "replaced": false }),
            false,
        )),
        (Some(a), Some(b)) if a != b => out.push((
            types::SECURITY_PASSWORD_SET,
            json!({ "replaced": true }),
            false,
        )),
        (Some(_), None) => out.push((types::SECURITY_PASSWORD_CLEARED, json!({}), false)),
        _ => {}
    }
    if before.auto_lock_minutes != after.auto_lock_minutes {
        out.push((
            types::SECURITY_AUTO_LOCK_CHANGED,
            json!({ "from": before.auto_lock_minutes, "to": after.auto_lock_minutes }),
            false,
        ));
    }
    if before.events != after.events {
        let ids = |c: &crate::events::EventsConfig| -> Vec<String> {
            c.subscriptions.iter().map(|s| s.id.clone()).collect()
        };
        let (old, new) = (ids(&before.events), ids(&after.events));
        let added: Vec<&String> = new.iter().filter(|i| !old.contains(i)).collect();
        let removed: Vec<&String> = old.iter().filter(|i| !new.contains(i)).collect();
        let changed: Vec<&String> = new
            .iter()
            .filter(|i| {
                let a = before.events.subscriptions.iter().find(|s| &s.id == *i);
                let b = after.events.subscriptions.iter().find(|s| &s.id == *i);
                matches!((a, b), (Some(a), Some(b)) if a != b)
            })
            .collect();
        out.push((
            types::EVENTS_CONFIG_CHANGED,
            json!({ "added": added, "removed": removed, "changed": changed }),
            true,
        ));
    }
    let kid = |c: &AdminConfig| c.receipt_key.as_ref().map(|k| k.kid.clone());
    if before.receipt_key != after.receipt_key {
        out.push((
            types::RECEIPTS_KEY_CHANGED,
            json!({ "kid": kid(after), "previousKid": kid(before) }),
            false,
        ));
    }
    out
}

/// Who hears `events.config_changed`: every subscription **as it was** — so one
/// that was removed, disabled in place or repointed is told at its old address,
/// with its old secret — plus any that is new.
pub fn config_change_audience(
    before: &[Subscription],
    after: &[Subscription],
) -> Vec<Subscription> {
    let mut out: Vec<Subscription> = before.to_vec();
    for s in after {
        if !out.iter().any(|o| o.id == s.id) {
            out.push(s.clone());
        }
    }
    out
}

/// What the root CLI sends itself the moment it rewrites the subscriptions, signed
/// and direct — it holds the secrets and does not depend on the app running, being
/// restarted, or still knowing a removed subscription. Answers one line per target.
/// The running app announces the same change when it notices; a receiver may see
/// it twice, which is harmless for a notice that only ever lowers trust.
pub async fn announce_config_change(before: &AdminConfig, after: &AdminConfig) -> Vec<String> {
    let Some((_, data, _)) = admin_changes(before, after)
        .into_iter()
        .find(|(ty, _, _)| *ty == types::EVENTS_CONFIG_CHANGED)
    else {
        return Vec::new();
    };
    let Ok(client) = http_client() else {
        return vec!["no HTTP client; nothing announced".into()];
    };
    // The CLI cannot see the settings of the account the app runs as, so it reports
    // the half of the posture it can read and says where the message came from.
    let posture = json!({
        "passwordSet": after.password_hash.is_some(),
        "autoLockMinutes": after.auto_lock_minutes,
        "source": "posd admin",
    });
    let body = envelope(
        types::EVENTS_CONFIG_CHANGED,
        &chrono::Utc::now().to_rfc3339(),
        after.events.device_id.as_deref(),
        posture,
        data,
    );
    let mut lines = Vec::new();
    for sub in config_change_audience(&before.events.subscriptions, &after.events.subscriptions)
        .into_iter()
        .filter(|s| s.enabled && matches(&s.events, types::EVENTS_CONFIG_CHANGED))
    {
        let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
        let outcome = send_signed(&client, &sub.url, &msg_id, &body, &sub.secret).await;
        lines.push(match outcome {
            Ok(status) if (200..300).contains(&status) => format!("{}  told ({status})", sub.id),
            Ok(status) => format!("{}  answered {status}", sub.id),
            Err(e) => format!("{}  not reached: {e}", sub.id),
        });
    }
    lines
}

/// The background half: notices what no HTTP handler sees — the administrator
/// file changing under `sudo posd admin`, settings changing from any route — and
/// delivers. Runs for the life of the app.
pub async fn run(state: AppState) {
    let bus = state.events.clone();
    let mut admin_before = state.auth.admin();
    bus.remember(&admin_before.events.subscriptions);
    bus.emit(
        &state,
        types::APP_STARTED,
        json!({ "version": env!("CARGO_PKG_VERSION") }),
    );

    let mut rx = state.settings.subscribe();
    let mut settings_before: Settings = state.settings.get();
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    loop {
        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let next = rx.borrow_and_update().clone();
                let paths = changed_paths(
                    &serde_json::to_value(&settings_before).unwrap_or(Value::Null),
                    &serde_json::to_value(&next).unwrap_or(Value::Null),
                );
                if !paths.is_empty() {
                    bus.emit(&state, types::SETTINGS_CHANGED, json!({
                        "changed": paths,
                        "previousSecurityDigest": security_digest(&settings_before),
                    }));
                }
                settings_before = next;
            }
            _ = tick.tick() => {
                // A file caught mid-write, or broken by hand, is not a change: skip
                // the tick rather than announce a cleared password that never was.
                let admin_now = match state.auth.try_admin() {
                    Ok(admin) => admin,
                    Err(e) => {
                        tracing::warn!("administrator file unreadable this tick ({e}); no events derived");
                        bus.deliver_due().await;
                        continue;
                    }
                };
                if admin_now != admin_before {
                    bus.remember(&admin_now.events.subscriptions);
                    for (ty, data, to_old_too) in admin_changes(&admin_before, &admin_now) {
                        if to_old_too {
                            let audience = config_change_audience(&admin_before.events.subscriptions, &admin_now.events.subscriptions);
                            bus.emit_to(&state, &admin_now, &audience, ty, data);
                        } else {
                            bus.emit_to(&state, &admin_now, &admin_now.events.subscriptions, ty, data);
                        }
                    }
                    admin_before = admin_now;
                }
                bus.deliver_due().await;
            }
        }
    }
}

/// Best effort on the way out: announce `app.stopping` and give it **at most**
/// `within` — a hard bound, since the desktop shell waits on this before it quits.
/// Only `app.stopping` is sent here; anything else waits in the outbox for the next
/// start rather than holding the window open.
pub async fn shutdown(state: &AppState, within: Duration) {
    state.events.emit(state, types::APP_STOPPING, json!({}));
    let bus = state.events.clone();
    let _ = tokio::time::timeout(within, async move {
        while bus.pending_of(types::APP_STOPPING) > 0 {
            bus.deliver_where(|d| d.event_type == types::APP_STOPPING)
                .await;
            if bus.pending_of(types::APP_STOPPING) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
}

/// The non-secret view of the configuration, for `GET /api/v1/events`.
pub fn subscription_views(admin: &AdminConfig) -> Vec<SubscriptionView> {
    admin
        .events
        .subscriptions
        .iter()
        .map(SubscriptionView::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example from the Standard Webhooks specification, reproduced
    /// independently with Python's `hmac` on 2026-10-13.
    #[test]
    fn signature_matches_the_standard_webhooks_example() {
        assert_eq!(
            sign(
                "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
                "msg_p5jXN8AQM9LWM0D4loKWxJek",
                1614265330,
                r#"{"test": 2432232314}"#
            )
            .unwrap(),
            "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE="
        );
    }

    #[test]
    fn patterns_match_exactly_by_prefix_or_all() {
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(matches(&p(&["*"]), "settings.changed"));
        assert!(matches(&p(&["security.*"]), "security.unlocked"));
        assert!(!matches(&p(&["security.*"]), "settings.changed"));
        assert!(!matches(&p(&["security.*"]), "securityx.y"));
        assert!(matches(&p(&["settings.changed"]), "settings.changed"));
        assert!(!matches(&p(&[]), "app.started"));
    }

    #[test]
    fn backoff_doubles_from_five_seconds_and_caps_at_fifteen_minutes() {
        assert_eq!(backoff_secs(0), 5);
        assert_eq!(backoff_secs(1), 10);
        assert_eq!(backoff_secs(2), 20);
        assert_eq!(backoff_secs(8), 900);
        assert_eq!(backoff_secs(40), 900);
    }

    #[test]
    fn generated_secrets_are_whsec_and_distinct() {
        let a = generate_secret().unwrap();
        assert!(a.starts_with("whsec_"));
        assert_ne!(a, generate_secret().unwrap());
        assert!(sign(&a, "m", 1, "{}").unwrap().starts_with("v1,"));
    }

    fn sub(id: &str, url: &str) -> Subscription {
        Subscription {
            id: id.into(),
            url: url.into(),
            secret: generate_secret().unwrap(),
            events: all_events(),
            enabled: true,
        }
    }

    #[test]
    fn admin_changes_name_what_moved() {
        let before = AdminConfig::default();
        let mut after = before.clone();
        after.password_hash = Some("h1".into());
        after.auto_lock_minutes = 5;
        after.events.subscriptions.push(sub("s1", "https://a"));
        let types: Vec<&str> = admin_changes(&before, &after).iter().map(|c| c.0).collect();
        assert_eq!(
            types,
            vec![
                types::SECURITY_PASSWORD_SET,
                types::SECURITY_AUTO_LOCK_CHANGED,
                types::EVENTS_CONFIG_CHANGED
            ]
        );

        let mut cleared = after.clone();
        cleared.password_hash = None;
        let c = admin_changes(&after, &cleared);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, types::SECURITY_PASSWORD_CLEARED);
        assert!(admin_changes(&after, &after).is_empty());
    }

    #[test]
    fn a_config_change_is_addressed_to_the_old_endpoints_too() {
        let before = vec![sub("s1", "https://old")];
        let after = vec![sub("s2", "https://new")];
        let audience: Vec<String> = config_change_audience(&before, &after)
            .into_iter()
            .map(|s| s.url)
            .collect();
        assert_eq!(
            audience,
            vec!["https://old".to_string(), "https://new".to_string()]
        );
    }

    #[test]
    fn the_envelope_carries_type_time_and_the_device() {
        let body: Value = serde_json::from_str(&envelope(
            types::SECURITY_UNLOCKED,
            "2026-10-13T00:00:00Z",
            Some("dev-1"),
            json!({ "passwordSet": true }),
            json!({ "x": 1 }),
        ))
        .unwrap();
        assert_eq!(body["type"], types::SECURITY_UNLOCKED);
        assert_eq!(body["data"]["x"], 1);
        assert_eq!(body["data"]["device"]["id"], "dev-1");
        assert_eq!(body["data"]["device"]["posture"]["passwordSet"], true);
    }

    #[test]
    fn urls_are_shown_without_path_query_or_credentials() {
        assert_eq!(
            redact_url("https://hooks.example.com"),
            "https://hooks.example.com"
        );
        assert_eq!(
            redact_url("https://hooks.example.com/services/T0/B0/secret"),
            "https://hooks.example.com/…"
        );
        assert_eq!(
            redact_url("https://user:pw@h.example:8443/x?token=1"),
            "https://h.example:8443/…"
        );
        assert_eq!(
            redact_url("http://127.0.0.1:9/hook"),
            "http://127.0.0.1:9/…"
        );
        assert_eq!(redact_url("https://h.example?t=1"), "https://h.example/…");
        assert_eq!(redact_url("not a url"), "…");
    }

    #[test]
    fn a_corrupt_outbox_is_kept_aside_not_silently_dropped() {
        let dir = std::env::temp_dir().join(format!("ledger-pos-outbox-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events-outbox.json");
        std::fs::write(&path, "[{\"msgId\": tru").unwrap();
        let bus = EventBus::new(Some(path.clone()));
        assert_eq!(bus.pending(), 0);
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.invalid")).unwrap(),
            "[{\"msgId\": tru"
        );
    }

    #[test]
    fn a_malformed_secret_is_refused_without_echoing_it() {
        let err = sign("whsec_not*base64", "m", 1, "{}").unwrap_err();
        assert!(!err.contains('*'), "{err}");
    }
}

//! What a receiver needs to judge this device's word: its lock posture, and a
//! fingerprint of the settings that decide where a payment goes.
//!
//! Every webhook event and every signed receipt carries both, so a server that
//! recorded the fingerprint when it accepted the device can tell — from any later
//! message — that the device is no longer configured the way it was accepted. That
//! includes a `settings.json` edited by hand while the app was not running: the
//! file is only read at start-up, so no event fires for the edit itself, but the
//! very next message carries a different fingerprint.

use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use crate::security::AuthState;
use crate::settings::Settings;

/// The settings a fingerprint covers: where the API listens, which browser origins
/// may call it, which terminal is active and how it is reached.
///
/// Shell behaviour (tray, start-at-boot, the install prompt) is left out on
/// purpose: it changes nothing a payment depends on, and a fingerprint that moved
/// with it would raise alarms nobody can act on.
pub fn security_view(s: &Settings) -> Value {
    let mut origins = s.server.cors_allowed_origins.clone();
    origins.sort();
    origins.dedup();
    json!({
        "server": {
            "host": s.server.host,
            "port": s.server.port,
            "corsAllowAll": s.server.cors_allow_all,
            "corsAllowedOrigins": origins,
        },
        "provider": {
            "active": s.providers.active,
            "config": s.providers.configs.get(&s.providers.active).cloned().unwrap_or(Value::Null),
        },
    })
}

/// `sha256:<hex>` over the canonical form of [`security_view`].
pub fn security_digest(s: &Settings) -> String {
    let canonical = canonical_json(&security_view(s));
    format!("sha256:{}", hex(&Sha256::digest(canonical.as_bytes())))
}

/// JSON with every object's keys sorted, recursively.
///
/// Not `serde_json::to_string`: a dependency in this tree enables serde_json's
/// `preserve_order`, so maps keep insertion order and the same settings could
/// serialize two ways — a fingerprint that changed with nothing changed.
pub fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => other.to_string(),
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The dotted paths at which two settings documents differ — names only, never
/// values: an event announces *that* the terminal address changed, not the address.
pub fn changed_paths(before: &Value, after: &Value) -> Vec<String> {
    let mut out = Vec::new();
    diff(before, after, String::new(), &mut out);
    out.sort();
    out
}

fn diff(a: &Value, b: &Value, path: String, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                diff(
                    x.get(k).unwrap_or(&Value::Null),
                    y.get(k).unwrap_or(&Value::Null),
                    child,
                    out,
                );
            }
        }
        _ if a != b => out.push(path),
        _ => {}
    }
}

/// The device's security posture at one moment.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Posture {
    /// An application password is set.
    pub password_set: bool,
    /// Protected changes can be made right now without a password: either no
    /// password is set, or a session is unlocked. A receiver that trusts this
    /// device's word should want `passwordSet && !unlocked`.
    pub unlocked: bool,
    pub auto_lock_minutes: u32,
    /// See [`security_digest`].
    pub security_digest: String,
    pub active_provider: String,
}

pub fn posture(auth: &AuthState, settings: &Settings) -> Posture {
    let admin = auth.admin();
    Posture {
        password_set: admin.password_hash.is_some(),
        unlocked: auth.any_unlocked(),
        auto_lock_minutes: admin.auto_lock_minutes,
        security_digest: security_digest(settings),
        active_provider: settings.providers.active.clone(),
    }
}

/// The scalar fields of a JSON object, nested objects flattened one level as
/// `parent.child` — the shape a signed receipt records of a request and a result,
/// whatever provider produced them.
pub fn scalars(v: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    if let Value::Object(map) = v {
        for (k, val) in map {
            match val {
                Value::Object(inner) => {
                    for (ik, iv) in inner {
                        if !iv.is_object() && !iv.is_array() {
                            out.insert(format!("{k}.{ik}"), iv.clone());
                        }
                    }
                }
                Value::Array(_) => {}
                other => {
                    out.insert(k.clone(), other.clone());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        let mut s = Settings::default();
        s.providers.active = "sandbox".into();
        s.providers.configs.insert(
            "sandbox".into(),
            json!({ "failRate": 0, "terminalId": "SBX00001" }),
        );
        s
    }

    #[test]
    fn the_digest_is_stable_and_ignores_key_order() {
        let a = settings();
        let mut b = settings();
        // Same config, keys inserted the other way round.
        b.providers.configs.insert(
            "sandbox".into(),
            json!({ "terminalId": "SBX00001", "failRate": 0 }),
        );
        assert_eq!(security_digest(&a), security_digest(&b));
        assert!(security_digest(&a).starts_with("sha256:"));
    }

    #[test]
    fn the_digest_moves_with_what_a_payment_depends_on() {
        let base = security_digest(&settings());
        let mut cors = settings();
        cors.server.cors_allow_all = true;
        let mut terminal = settings();
        terminal.providers.configs.insert(
            "sandbox".into(),
            json!({ "failRate": 0, "terminalId": "OTHER" }),
        );
        let mut port = settings();
        port.server.port = 9999;
        assert_ne!(base, security_digest(&cors));
        assert_ne!(base, security_digest(&terminal));
        assert_ne!(base, security_digest(&port));
    }

    #[test]
    fn the_digest_ignores_shell_behaviour_and_origin_order() {
        let base = security_digest(&settings());
        let mut shell = settings();
        shell.app.start_at_boot = !shell.app.start_at_boot;
        let mut reordered = settings();
        reordered.server.cors_allowed_origins.reverse();
        assert_eq!(base, security_digest(&shell));
        assert_eq!(base, security_digest(&reordered));
    }

    #[test]
    fn changed_paths_names_the_leaves_and_never_the_values() {
        let a = serde_json::to_value(settings()).unwrap();
        let mut s = settings();
        s.server.cors_allow_all = true;
        s.providers.configs.insert(
            "sandbox".into(),
            json!({ "failRate": 1, "terminalId": "SBX00001" }),
        );
        let b = serde_json::to_value(s).unwrap();
        assert_eq!(
            changed_paths(&a, &b),
            vec![
                "providers.configs.sandbox.failRate".to_string(),
                "server.corsAllowAll".to_string()
            ]
        );
        assert!(changed_paths(&a, &a).is_empty());
    }

    #[test]
    fn scalars_flatten_one_level_and_drop_collections() {
        let v = json!({ "ok": true, "rrn": "1", "result": { "responseCode": "00", "deep": { "x": 1 } }, "list": [1] });
        let s = scalars(&v);
        assert_eq!(s.get("ok"), Some(&json!(true)));
        assert_eq!(s.get("rrn"), Some(&json!("1")));
        assert_eq!(s.get("result.responseCode"), Some(&json!("00")));
        assert!(!s.contains_key("result.deep") && !s.contains_key("list"));
    }

    #[test]
    fn canonical_json_sorts_keys_recursively() {
        assert_eq!(
            canonical_json(&json!({ "b": 1, "a": { "d": 2, "c": [ { "f": 1, "e": 2 } ] } })),
            r#"{"a":{"c":[{"e":2,"f":1}],"d":2},"b":1}"#
        );
    }
}

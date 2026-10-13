//! Signed receipts — optional, off until an administrator sets a key.
//!
//! The REST API answers whoever asks on this machine, and whatever it answers
//! usually travels on through a browser or a till to a server that has to decide
//! whether the sale happened. Without a signature that server can only check the
//! receipt's *consistency*; anything on the path could have written it.
//!
//! With a key set (`sudo posd admin receipt-key set`), every successful `invoke`
//! reply gains an `attestation` field: a standard JWS compact token (HS256) whose
//! claims are what was asked, what the terminal answered, and the device's
//! [posture](crate::posture::Posture) at that moment. Any JWT library verifies it
//! with the shared key; the claims, not the surrounding JSON, are what a receiver
//! should trust, so no canonical form of the provider's reply is needed.
//!
//! The key is only as private as the account the app runs as — see the README's
//! *Locking the app* for running the daemon under its own account.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::posture::{scalars, Posture};

/// `typ` of the token's header, so a receiver can refuse a token minted for
/// something else under the same key.
pub const TOKEN_TYPE: &str = "ledger-pos-receipt+jwt";
pub const ISSUER: &str = "ledger-pos";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptKey {
    /// Key id, carried in the token header so a receiver holding several keys
    /// (several devices, or one device mid-rotation) knows which to use.
    pub kid: String,
    /// 32 random bytes, base64url without padding.
    pub secret: String,
}

impl ReceiptKey {
    pub fn generate(kid: Option<String>) -> Result<Self, String> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| format!("no randomness available: {e}"))?;
        let kid = kid
            .unwrap_or_else(|| format!("rk_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]));
        Ok(Self {
            kid,
            secret: URL_SAFE_NO_PAD.encode(bytes),
        })
    }

    fn key_bytes(&self) -> Result<Vec<u8>, String> {
        URL_SAFE_NO_PAD
            .decode(self.secret.trim_end_matches('='))
            .map_err(|e| format!("receipt key is not base64url: {e}"))
    }
}

/// A JWS compact token, HS256, over `claims` exactly as serialized here.
pub fn sign_jws(key: &ReceiptKey, claims: &Value) -> Result<String, String> {
    let header = json!({ "alg": "HS256", "typ": TOKEN_TYPE, "kid": key.kid });
    jws_raw(
        &key.key_bytes()?,
        &serde_json::to_string(&header).expect("header"),
        &serde_json::to_string(claims).expect("claims"),
    )
}

/// The token over exactly these header and claims bytes. A verifier works from the
/// bytes inside the token, so their key order is whatever serialization produced —
/// which is why the test vector below pins bytes rather than parsed values.
fn jws_raw(key: &[u8], header: &str, claims: &str) -> Result<String, String> {
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header),
        URL_SAFE_NO_PAD.encode(claims)
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|e| e.to_string())?;
    mac.update(signing_input.as_bytes());
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    ))
}

/// The claims of one receipt.
///
/// `request` and `response` are the scalar fields of what was sent and what came
/// back (one nested level flattened as `parent.child`), so the same shape serves
/// every provider without this module knowing any of them.
#[allow(clippy::too_many_arguments)]
pub fn receipt_claims(
    device_id: Option<&str>,
    provider: &str,
    function: &str,
    params: &Value,
    result: &Value,
    posture: &Posture,
    issued_at: i64,
    jti: &str,
) -> Value {
    let mut claims = json!({
        "iss": ISSUER,
        "iat": issued_at,
        "jti": jti,
        "provider": provider,
        "function": function,
        "request": Value::Object(scalars(params)),
        "response": Value::Object(scalars(result)),
        "posture": posture,
    });
    if let Some(device) = device_id {
        claims["sub"] = json!(device);
    }
    claims
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Computed independently with Python's `hmac` + `base64` (2026-10-13), so this
    /// pins the encoding rather than agreeing with itself.
    #[test]
    fn hs256_matches_an_independent_implementation() {
        let key: Vec<u8> = (0u8..32).collect();
        assert_eq!(
            jws_raw(&key, r#"{"alg":"HS256","typ":"JWT","kid":"k1"}"#, r#"{"iat":1700000000,"rrn":"123"}"#).unwrap(),
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6ImsxIn0.eyJpYXQiOjE3MDAwMDAwMDAsInJybiI6IjEyMyJ9.SmUne1MpoANvptFEpJj4ghJjqO_LS6N-qDWsegvLoGI"
        );
    }

    fn posture() -> Posture {
        Posture {
            password_set: true,
            unlocked: false,
            auto_lock_minutes: 15,
            security_digest: "sha256:abc".into(),
            active_provider: "sandbox".into(),
        }
    }

    #[test]
    fn a_receipt_carries_request_response_posture_and_key_id() {
        let key = ReceiptKey::generate(Some("dev-1".into())).unwrap();
        let claims = receipt_claims(
            Some("device-9"),
            "sandbox",
            "purchase",
            &json!({ "mainAmount": 10000 }),
            &json!({ "ok": true, "responseCode": "00", "rrn": "463214536298" }),
            &posture(),
            1_700_000_000,
            "j1",
        );
        let token = sign_jws(&key, &claims).unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        let body: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(header["kid"], "dev-1");
        assert_eq!(header["typ"], TOKEN_TYPE);
        assert_eq!(body["sub"], "device-9");
        assert_eq!(body["request"]["mainAmount"], 10000);
        assert_eq!(body["response"]["rrn"], "463214536298");
        assert_eq!(body["posture"]["passwordSet"], true);
        assert_eq!(body["posture"]["unlocked"], false);
    }

    #[test]
    fn a_tampered_claim_breaks_the_signature() {
        let key = ReceiptKey::generate(None).unwrap();
        let token = sign_jws(&key, &json!({ "amount": 1 })).unwrap();
        let forged = sign_jws(&key, &json!({ "amount": 2 })).unwrap();
        let spliced = format!(
            "{}.{}.{}",
            token.split('.').next().unwrap(),
            forged.split('.').nth(1).unwrap(),
            token.split('.').nth(2).unwrap()
        );
        let parts: Vec<&str> = spliced.split('.').collect();
        let mut mac = Hmac::<Sha256>::new_from_slice(&key.key_bytes().unwrap()).unwrap();
        mac.update(format!("{}.{}", parts[0], parts[1]).as_bytes());
        assert!(mac
            .verify_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap())
            .is_err());
    }

    #[test]
    fn generated_keys_are_distinct_and_32_bytes() {
        let a = ReceiptKey::generate(None).unwrap();
        let b = ReceiptKey::generate(None).unwrap();
        assert_ne!(a.secret, b.secret);
        assert_ne!(a.kid, b.kid);
        assert_eq!(a.key_bytes().unwrap().len(), 32);
    }
}

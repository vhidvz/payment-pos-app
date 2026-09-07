//! Sandbox provider — a terminal simulator that mirrors the Saman SSP1126
//! function catalog exactly, so API consumers and the UI can be exercised
//! end-to-end without hardware. Latency and failure behavior are configurable.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::saman::bill;
use super::saman::client::{CardSegment, TransactionResult};
use super::saman::invoke_local_utility;
use super::{
    terminal_function_specs, FunctionSpec, Provider, ProviderError, ProviderMetadata,
    ProviderStatus,
};

pub const PROVIDER_ID: &str = "sandbox";

fn d_latency() -> u64 { 250 }
fn d_card_delay() -> u64 { 1500 }
fn d_terminal_id() -> String { "SBX00001".into() }
fn d_balance() -> u64 { 12_345_000 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct SandboxConfig {
    /// Simulated network/terminal latency per step (ms).
    pub latency_ms: u64,
    /// Extra delay simulating the customer presenting a card (ms).
    pub card_present_delay_ms: u64,
    /// Probability [0..1] that a card transaction declines.
    pub fail_rate: f64,
    /// Terminal id reported in results.
    pub terminal_id: String,
    /// Balance reported by balance().
    pub balance_rials: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            latency_ms: d_latency(),
            card_present_delay_ms: d_card_delay(),
            fail_rate: 0.0,
            terminal_id: d_terminal_id(),
            balance_rials: d_balance(),
        }
    }
}

struct SandboxState {
    cfg: SandboxConfig,
    init_pending: bool,
    stan: u64,
    rng: u64,
}

impl SandboxState {
    fn next_rand(&mut self) -> u64 {
        // xorshift64* — plenty for a simulator
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn rand_digits(&mut self, n: usize) -> String {
        (0..n).map(|_| char::from(b'0' + (self.next_rand() % 10) as u8)).collect()
    }

    fn roll_decline(&mut self) -> Option<(&'static str, &'static str)> {
        let roll = (self.next_rand() % 10_000) as f64 / 10_000.0;
        if roll < self.cfg.fail_rate {
            let codes = [
                ("51", "Insufficient balance"),
                ("55", "Invalid card PIN"),
                ("98", "Operation cancelled by user"),
                ("33", "Card's expiration date has passed"),
            ];
            Some(codes[(self.next_rand() % codes.len() as u64) as usize])
        } else {
            None
        }
    }

    fn base_ok(&mut self) -> TransactionResult {
        self.stan += 1;
        let now = chrono::Local::now();
        TransactionResult {
            ok: true,
            response_code: Some("00".into()),
            response_message: Some("Transaction completed successfully".into()),
            terminal_id: Some(self.cfg.terminal_id.clone()),
            trace_number: Some(format!("{:06}", self.stan)),
            serial_id: Some(format!("{:06}", 900_000 + self.stan)),
            rrn: Some(self.rand_digits(12)),
            transaction_date: Some(now.format("%y%m%d%H%M%S").to_string()),
            card_mask: Some("603799******1234".into()),
            card_hash1: Some(self.rand_digits(40)),
            card_hash2: Some(self.rand_digits(40)),
            ..Default::default()
        }
    }

    fn declined(&mut self, code: &str, message: &str) -> TransactionResult {
        TransactionResult {
            ok: false,
            response_code: Some(code.into()),
            response_message: Some(message.into()),
            terminal_id: Some(self.cfg.terminal_id.clone()),
            timed_out: Some(false),
            ..Default::default()
        }
    }
}

pub struct SandboxProvider {
    state: Arc<Mutex<SandboxState>>,
}

impl SandboxProvider {
    pub fn new(config: Value) -> Self {
        let cfg = match config {
            Value::Null => SandboxConfig::default(),
            other => serde_json::from_value(other).unwrap_or_else(|e| {
                tracing::warn!("stored sandbox config is invalid ({e}); using defaults");
                SandboxConfig::default()
            }),
        };
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15)
            | 1;
        Self {
            state: Arc::new(Mutex::new(SandboxState {
                cfg,
                init_pending: false,
                stan: 0,
                rng: seed,
            })),
        }
    }
}

fn to_json<T: serde::Serialize>(v: T) -> Result<Value, ProviderError> {
    serde_json::to_value(v).map_err(|e| ProviderError::Execution(e.to_string()))
}

#[async_trait]
impl Provider for SandboxProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: PROVIDER_ID.into(),
            name: "Sandbox terminal".into(),
            vendor: "Ledger POS (built-in)".into(),
            model: Some("Simulator".into()),
            description: "A built-in terminal simulator that mirrors the Saman SSP1126 function catalog \
                          one-for-one. Use it to develop and demo API integrations, UI flows and receipts \
                          without hardware. Latency, card-present delay and decline rate are configurable."
                .into(),
            protocol: Some("In-memory simulation (no wire protocol)".into()),
            transports: vec!["virtual".into()],
            capabilities: vec![
                "purchase".into(),
                "pos-started-purchase".into(),
                "bill-payment".into(),
                "pin-charge".into(),
                "topup-charge".into(),
                "balance".into(),
                "bill-inquiry".into(),
                "reports".into(),
                "simulation".into(),
            ],
            docs_url: None,
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    fn functions(&self) -> Vec<FunctionSpec> {
        terminal_function_specs()
    }

    fn config_schema(&self) -> Value {
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Sandbox terminal configuration",
            "type": "object",
            "properties": {
                "latencyMs": { "type": "integer", "default": 250, "minimum": 0, "description": "Simulated latency per operation (ms)." },
                "cardPresentDelayMs": { "type": "integer", "default": 1500, "minimum": 0, "description": "Extra delay simulating the customer presenting a card (ms)." },
                "failRate": { "type": "number", "default": 0, "minimum": 0, "maximum": 1, "description": "Probability [0..1] that a card transaction declines." },
                "terminalId": { "type": "string", "default": "SBX00001", "description": "Terminal id reported in results." },
                "balanceRials": { "type": "integer", "default": 12345000, "minimum": 0, "description": "Balance reported by the balance inquiry." }
            }
        })
    }

    async fn get_config(&self) -> Value {
        let s = self.state.lock().await;
        serde_json::to_value(&s.cfg).unwrap_or_else(|_| json!({}))
    }

    async fn set_config(&self, config: Value) -> Result<(), ProviderError> {
        let cfg: SandboxConfig = serde_json::from_value(config)
            .map_err(|e| ProviderError::InvalidParams(format!("invalid configuration: {e}")))?;
        if !(0.0..=1.0).contains(&cfg.fail_rate) {
            return Err(ProviderError::InvalidParams("failRate must be between 0 and 1".into()));
        }
        let mut s = self.state.lock().await;
        s.cfg = cfg;
        Ok(())
    }

    async fn status(&self) -> ProviderStatus {
        let s = self.state.lock().await;
        ProviderStatus {
            configured: true,
            configuration_hint: None,
            session_open: s.init_pending,
            busy: false,
        }
    }

    async fn invoke(&self, function: &str, params: Value) -> Result<Value, ProviderError> {
        if let Some(res) = invoke_local_utility(function, &params) {
            return res;
        }

        let spec_exists = terminal_function_specs().iter().any(|f| f.id == function);
        if !spec_exists {
            return Err(ProviderError::UnknownFunction(function.to_string()));
        }

        let m = match &params {
            Value::Object(m) => m.clone(),
            Value::Null => serde_json::Map::new(),
            _ => {
                return Err(ProviderError::InvalidParams(
                    "request body must be a JSON object".into(),
                ))
            }
        };

        // Simulated latencies (kept outside the lock so parallel calls stay snappy).
        let (latency, card_delay) = {
            let s = self.state.lock().await;
            (s.cfg.latency_ms, s.cfg.card_present_delay_ms)
        };
        tokio::time::sleep(Duration::from_millis(latency)).await;
        let card_flow = matches!(
            function,
            "balance" | "purchase" | "posStarterPurchaseInit" | "billPayment" | "billRequest"
                | "pinCharge" | "topupCharge" | "mciBillInquiry" | "tciBillInquiry"
        );
        if card_flow {
            tokio::time::sleep(Duration::from_millis(card_delay)).await;
        }

        let mut s = self.state.lock().await;
        let req_amount = m.get("mainAmount").and_then(|v| v.as_u64());

        match function {
            "connectionTest" => {
                let mut r = s.base_ok();
                r.card_mask = None;
                r.card_hash1 = None;
                r.card_hash2 = None;
                r.rrn = None;
                to_json(r)
            }
            "getAuthorizedOperations" => {
                let mut result = s.base_ok();
                result.pos_version = Some("10.063.00PO (sandbox)".into());
                to_json(json!({
                    "balance": true, "bill": true, "report": true, "mciBill": true,
                    "pinCharge": true, "purchase": true, "topupCharge": true,
                    "tciBill": true, "paymentService": true,
                    "posVersion": "10.063.00PO (sandbox)",
                    "result": serde_json::to_value(result).unwrap_or_default()
                }))
            }
            "balance" => {
                if let Some((code, msg)) = s.roll_decline() {
                    return to_json(s.declined(code, msg));
                }
                let bal = s.cfg.balance_rials;
                let mut r = s.base_ok();
                r.amount = Some(bal.to_string());
                to_json(r)
            }
            "purchase" => {
                let amount = req_amount
                    .ok_or_else(|| ProviderError::InvalidParams("'mainAmount' is required".into()))?;
                // Mirror the real terminal's behavior for identified purchases: an
                // unprovisioned DE63 request declines with 07 before any card prompt.
                if m.get("purchaseId").map(|v| !v.is_null()).unwrap_or(false) {
                    return to_json(s.declined("07", "No permission for this operation"));
                }
                if let Some((code, msg)) = s.roll_decline() {
                    return to_json(s.declined(code, msg));
                }
                let mut r = s.base_ok();
                r.amount = Some(amount.to_string());
                r.effective_amount = Some(amount.to_string());
                to_json(r)
            }
            "posStarterPurchaseInit" => {
                if let Some((code, msg)) = s.roll_decline() {
                    let d = s.declined(code, msg);
                    let mut v = serde_json::to_value(d)
                        .map_err(|e| ProviderError::Execution(e.to_string()))?;
                    v["segments"] = json!([]);
                    return Ok(v);
                }
                s.init_pending = true;
                let r = s.base_ok();
                let mut v = serde_json::to_value(r).map_err(|e| ProviderError::Execution(e.to_string()))?;
                v["segments"] = serde_json::to_value(vec![CardSegment {
                    code: "00".into(),
                    label: "Saman".into(),
                }])
                .unwrap_or_default();
                Ok(v)
            }
            "posStarterPurchaseFin" => {
                let amount = req_amount
                    .ok_or_else(|| ProviderError::InvalidParams("'mainAmount' is required".into()))?;
                if !s.init_pending {
                    return to_json(s.declined("12", "Invalid transaction"));
                }
                s.init_pending = false;
                if let Some((code, msg)) = s.roll_decline() {
                    return to_json(s.declined(code, msg));
                }
                let mut r = s.base_ok();
                r.amount = Some(amount.to_string());
                r.effective_amount = Some(amount.to_string());
                to_json(r)
            }
            "posStarterPurchaseCancel" => {
                s.init_pending = false;
                Ok(json!({ "ok": true }))
            }
            "billPayment" => {
                let bill_id = m.get("billId").and_then(|v| v.as_str()).unwrap_or("");
                let payment_id = m.get("paymentId").and_then(|v| v.as_str()).unwrap_or("");
                let validation = bill::validate_bill(bill_id, payment_id);
                if !validation.ok {
                    return to_json(s.declined("04", "Invalid information"));
                }
                if let Some((code, msg)) = s.roll_decline() {
                    return to_json(s.declined(code, msg));
                }
                let amount = validation.amount_rials.unwrap_or(0);
                let mut r = s.base_ok();
                r.amount = Some(amount.to_string());
                r.effective_amount = Some(amount.to_string());
                to_json(r)
            }
            "billRequest" => {
                if let Some((code, msg)) = s.roll_decline() {
                    return to_json(s.declined(code, msg));
                }
                let mut r = s.base_ok();
                let bill_id = format!("{}20", s.rand_digits(9));
                let payment_id = s.rand_digits(9);
                r.fields.insert(60, bill_id);
                r.fields.insert(61, payment_id);
                to_json(r)
            }
            "pinCharge" => {
                if let Some((code, msg)) = s.roll_decline() {
                    return to_json(s.declined(code, msg));
                }
                let mut r = s.base_ok();
                r.amount = Some("100000".into());
                r.effective_amount = Some("100000".into());
                r.charge_pin = Some(s.rand_digits(12));
                r.charge_serial = Some(s.rand_digits(10));
                r.charge_emergency_number = Some("09990000000".into());
                to_json(r)
            }
            "topupCharge" => {
                if m.get("mobileNumber").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(ProviderError::InvalidParams("'mobileNumber' is required".into()));
                }
                if let Some((code, msg)) = s.roll_decline() {
                    return to_json(s.declined(code, msg));
                }
                let mut r = s.base_ok();
                r.amount = Some("50000".into());
                r.effective_amount = Some("50000".into());
                to_json(r)
            }
            "mciBillInquiry" | "tciBillInquiry" => {
                if let Some((code, msg)) = s.roll_decline() {
                    return to_json(s.declined(code, msg));
                }
                let mut r = s.base_ok();
                let due = 100_000 + (s.next_rand() % 900) * 1000;
                r.effective_amount = Some(due.to_string());
                to_json(r)
            }
            "totalReport" => {
                let result = s.base_ok();
                let purchase_count = 3 + s.next_rand() % 20;
                let purchase_amount = purchase_count * 150_000;
                to_json(json!({
                    "billsCount": 2, "billsTotalAmount": 830000,
                    "pinChargeCount": 1, "pinChargeAmount": 100000,
                    "topupChargeCount": 4, "topupChargeAmount": 200000,
                    "purchaseCount": purchase_count, "purchaseAmount": purchase_amount,
                    "groupChargeCount": 0, "groupChargeAmount": 0,
                    "result": serde_json::to_value(result).unwrap_or_default()
                }))
            }
            "report" => {
                let result = s.base_ok();
                let n = 3 + (s.next_rand() % 5) as usize;
                let mut rows = Vec::with_capacity(n);
                for i in 0..n {
                    let amount = 50_000 + (s.next_rand() % 500) * 1000;
                    rows.push(json!({
                        "date": chrono::Local::now().format("%y%m%d").to_string(),
                        "shiftCode": "01",
                        "traceNumber": format!("{:06}", 100 + i),
                        "amount": amount,
                        "rrn": s.rand_digits(12),
                        "bank": "Saman",
                        "cardMask": "603799******1234",
                        "cardHash": s.rand_digits(40),
                        "terminalId": s.cfg.terminal_id.clone(),
                    }));
                }
                to_json(json!({
                    "rows": rows,
                    "result": serde_json::to_value(result).unwrap_or_default()
                }))
            }
            other => Err(ProviderError::UnknownFunction(other.to_string())),
        }
    }
}

#[cfg(test)]
mod sandbox_link_tests {
    use super::*;
    use crate::providers::LinkState;

    /// The simulator has no wire to be up or down on.
    #[tokio::test]
    async fn link_probe_is_not_applicable() {
        let status = SandboxProvider::new(Value::Null).probe_link().await;
        assert_eq!(status.state, LinkState::NotApplicable, "detail: {}", status.detail);
    }
}

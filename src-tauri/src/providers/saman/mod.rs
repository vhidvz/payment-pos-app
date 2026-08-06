//! Saman SSP1126 provider — exposes the full `saman-payment-pos` SDK surface
//! through the generic [`Provider`](super::Provider) interface.

pub mod bill;
pub mod client;
pub mod iso8583;
pub mod response_codes;
pub mod transport;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use self::client::{config_problem, PrintData, SamanClient, SamanConfig};
use super::{
    terminal_function_specs, FunctionSpec, Provider, ProviderError, ProviderMetadata,
    ProviderStatus,
};

pub const PROVIDER_ID: &str = "saman-ssp1126";

pub struct SamanProvider {
    client: Arc<Mutex<SamanClient>>,
}

impl SamanProvider {
    pub fn new(config: Value) -> Self {
        let cfg = match config {
            Value::Null => SamanConfig::default(),
            other => serde_json::from_value(other).unwrap_or_else(|e| {
                tracing::warn!("stored saman-ssp1126 config is invalid ({e}); using defaults");
                SamanConfig::default()
            }),
        };
        Self {
            client: Arc::new(Mutex::new(SamanClient::new(cfg))),
        }
    }
}

// ---------------------------------------------------------- param plumbing

fn as_object(params: &Value) -> Result<&serde_json::Map<String, Value>, ProviderError> {
    match params {
        Value::Object(m) => Ok(m),
        Value::Null => {
            static EMPTY: std::sync::OnceLock<serde_json::Map<String, Value>> =
                std::sync::OnceLock::new();
            Ok(EMPTY.get_or_init(serde_json::Map::new))
        }
        _ => Err(ProviderError::InvalidParams(
            "request body must be a JSON object".into(),
        )),
    }
}

fn req_str(m: &serde_json::Map<String, Value>, key: &str) -> Result<String, ProviderError> {
    match m.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        Some(Value::Number(n)) => Ok(n.to_string()),
        Some(_) => Err(ProviderError::InvalidParams(format!(
            "'{key}' must be a non-empty string"
        ))),
        None => Err(ProviderError::InvalidParams(format!("'{key}' is required"))),
    }
}

fn opt_str(m: &serde_json::Map<String, Value>, key: &str) -> Result<Option<String>, ProviderError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let t = s.trim();
            Ok(if t.is_empty() { None } else { Some(t.to_string()) })
        }
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(_) => Err(ProviderError::InvalidParams(format!(
            "'{key}' must be a string"
        ))),
    }
}

fn req_u64(m: &serde_json::Map<String, Value>, key: &str) -> Result<u64, ProviderError> {
    match m.get(key) {
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| ProviderError::InvalidParams(format!("'{key}' must be a non-negative integer"))),
        Some(Value::String(s)) => s
            .trim()
            .parse::<u64>()
            .map_err(|_| ProviderError::InvalidParams(format!("'{key}' must be a non-negative integer"))),
        Some(_) => Err(ProviderError::InvalidParams(format!(
            "'{key}' must be a non-negative integer"
        ))),
        None => Err(ProviderError::InvalidParams(format!("'{key}' is required"))),
    }
}

fn req_i64(m: &serde_json::Map<String, Value>, key: &str) -> Result<i64, ProviderError> {
    match m.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .ok_or_else(|| ProviderError::InvalidParams(format!("'{key}' must be an integer"))),
        Some(Value::String(s)) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| ProviderError::InvalidParams(format!("'{key}' must be an integer"))),
        Some(_) => Err(ProviderError::InvalidParams(format!("'{key}' must be an integer"))),
        None => Err(ProviderError::InvalidParams(format!("'{key}' is required"))),
    }
}

fn opt_print_data(
    m: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<PrintData>>, ProviderError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value::<Vec<PrintData>>(v.clone())
            .map(Some)
            .map_err(|e| {
                ProviderError::InvalidParams(format!(
                    "'{key}' must be an array of {{alignment, receipt, title, value}}: {e}"
                ))
            }),
    }
}

fn req_str_vec(m: &serde_json::Map<String, Value>, key: &str) -> Result<Vec<String>, ProviderError> {
    match m.get(key) {
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.clone()),
                Value::Number(n) => Ok(n.to_string()),
                _ => Err(ProviderError::InvalidParams(format!(
                    "'{key}' must be an array of strings"
                ))),
            })
            .collect(),
        Some(_) => Err(ProviderError::InvalidParams(format!(
            "'{key}' must be an array of strings"
        ))),
        None => Err(ProviderError::InvalidParams(format!("'{key}' is required"))),
    }
}

fn to_json<T: serde::Serialize>(v: T) -> Result<Value, ProviderError> {
    serde_json::to_value(v).map_err(|e| ProviderError::Execution(e.to_string()))
}

fn map_client_err(e: client::ClientError) -> ProviderError {
    match e {
        client::ClientError::Config(msg) => ProviderError::NotConfigured(msg),
        other => ProviderError::Execution(other.to_string()),
    }
}

/// Local (no-terminal) utility functions shared verbatim by the sandbox provider.
pub fn invoke_local_utility(function: &str, params: &Value) -> Option<Result<Value, ProviderError>> {
    let m = match as_object(params) {
        Ok(m) => m,
        Err(e) => return Some(Err(e)),
    };
    match function {
        "validateBill" => Some((|| {
            let bill_id = req_str(m, "billId")?;
            let payment_id = req_str(m, "paymentId")?;
            to_json(bill::validate_bill(&bill_id, &payment_id))
        })()),
        "billAmountRials" => Some((|| {
            let payment_id = req_str(m, "paymentId")?;
            Ok(json!({ "amountRials": bill::bill_amount_rials(&payment_id) }))
        })()),
        "billCategory" => Some((|| {
            let bill_id = req_str(m, "billId")?;
            to_json(bill::bill_category(&bill_id))
        })()),
        _ => None,
    }
}

#[async_trait]
impl Provider for SamanProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            id: PROVIDER_ID.into(),
            name: "Saman SSP1126".into(),
            vendor: "Saman Electronic Payment (SEP)".into(),
            model: Some("SSP1126".into()),
            description: "Drives the SSP1126 POS terminal over its native ISO-8583:1987 dialect — purchases \
                          (PC-started and POS-started), bill payment, PIN/top-up charge, balance, MCI/TCI bill \
                          inquiries and on-terminal reports. Ported from the saman-payment-pos SDK, including its \
                          measured reconnect-gap and first-ack retry behavior."
                .into(),
            protocol: Some(
                "ISO-8583:1987 over TCP or Serial (8-N-1). Asymmetric framing: requests raw, responses \
                 length-prefixed with a 5-byte header. DE64 single-DES CBC-MAC integrity."
                    .into(),
            ),
            transports: vec!["tcp".into(), "serial".into()],
            capabilities: vec![
                "purchase".into(),
                "pos-started-purchase".into(),
                "bill-payment".into(),
                "pin-charge".into(),
                "topup-charge".into(),
                "balance".into(),
                "bill-inquiry".into(),
                "reports".into(),
            ],
            docs_url: Some("https://github.com/vhidvz/saman-payment-pos".into()),
            version: "1.0.1".into(),
        }
    }

    fn functions(&self) -> Vec<FunctionSpec> {
        terminal_function_specs()
    }

    fn config_schema(&self) -> Value {
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Saman SSP1126 configuration",
            "type": "object",
            "properties": {
                "transport": { "type": "string", "enum": ["tcp", "serial"], "default": "tcp", "description": "Physical channel to the terminal." },
                "host": { "type": "string", "default": "", "description": "Terminal IP address (TCP).", "examples": ["192.168.14.105"] },
                "port": { "type": "integer", "default": 1197, "minimum": 1, "maximum": 65535, "description": "Terminal TCP port." },
                "path": { "type": "string", "default": "", "description": "Serial device path (serial).", "examples": ["COM3", "/dev/ttyUSB0"] },
                "baudRate": { "type": "integer", "default": 19200, "description": "Serial baud rate (8-N-1 fixed)." },
                "connectTimeoutMs": { "type": "integer", "default": 10000, "minimum": 100, "description": "Connect / open timeout (ms)." },
                "currency": { "type": "string", "default": "364", "description": "DE49 currency code (364 = IRR)." },
                "componentVersion": { "type": "string", "default": "1.4.3.0", "description": "DE57 component version reported on purchases." },
                "verifyMac": { "type": "boolean", "default": true, "description": "Verify the DE64 MAC on incoming messages." },
                "minReconnectGapMs": { "type": "integer", "default": 500, "minimum": 0, "description": "Reconnect gap after a transaction that ended cleanly (final 17)." },
                "reconnectGapAfterPartialMs": { "type": "integer", "default": 1500, "minimum": 0, "description": "Reconnect gap after a transaction without a final 17." },
                "firstAckTimeoutMs": { "type": "integer", "default": 3000, "minimum": 100, "description": "Timeout for the opening 15 acknowledgement." },
                "retryOnFirstTimeout": { "type": "boolean", "default": true, "description": "Retry once when the opening request times out on a fresh connection." },
                "retryDelayMs": { "type": "integer", "default": 500, "minimum": 0, "description": "Delay before that retry (ms)." },
                "includeTrace": { "type": "boolean", "default": false, "description": "Attach the raw hex frame trace to every result (debugging)." }
            }
        })
    }

    async fn get_config(&self) -> Value {
        let client = self.client.lock().await;
        serde_json::to_value(client.config()).unwrap_or_else(|_| json!({}))
    }

    async fn set_config(&self, config: Value) -> Result<(), ProviderError> {
        let cfg: SamanConfig = serde_json::from_value(config)
            .map_err(|e| ProviderError::InvalidParams(format!("invalid configuration: {e}")))?;
        if !matches!(cfg.transport.as_str(), "tcp" | "serial") {
            return Err(ProviderError::InvalidParams(
                "transport must be \"tcp\" or \"serial\"".into(),
            ));
        }
        let mut client = self.client.try_lock().map_err(|_| ProviderError::Busy)?;
        *client = SamanClient::new(cfg);
        Ok(())
    }

    async fn status(&self) -> ProviderStatus {
        match self.client.try_lock() {
            Ok(client) => ProviderStatus {
                configured: config_problem(client.config()).is_none(),
                configuration_hint: config_problem(client.config()),
                session_open: client.is_connected(),
                busy: false,
            },
            Err(_) => ProviderStatus {
                configured: true,
                configuration_hint: None,
                session_open: true,
                busy: true,
            },
        }
    }

    async fn invoke(&self, function: &str, params: Value) -> Result<Value, ProviderError> {
        // Local utilities never touch the terminal and never contend for the client.
        if let Some(res) = invoke_local_utility(function, &params) {
            return res;
        }

        let m = as_object(&params)?.clone();
        // A single client guards the terminal conversation; a second concurrent
        // invocation gets an immediate Busy rather than silently queueing behind a
        // (potentially 120s) card wait.
        let mut client = self.client.try_lock().map_err(|_| ProviderError::Busy)?;

        match function {
            "connectionTest" => client.connection_test().await.map_err(map_client_err).and_then(to_json),
            "getAuthorizedOperations" => client
                .get_authorized_operations()
                .await
                .map_err(map_client_err)
                .and_then(to_json),
            "balance" => client.balance().await.map_err(map_client_err).and_then(to_json),
            "purchase" => {
                let main_amount = req_u64(&m, "mainAmount")?;
                let amounts = opt_str(&m, "amounts")?;
                let purchase_id = opt_str(&m, "purchaseId")?;
                let terminal_id = opt_str(&m, "terminalId")?;
                let additional = opt_print_data(&m, "additionalData")?;
                let reference = opt_str(&m, "referenceData")?;
                client
                    .purchase(
                        main_amount,
                        amounts.as_deref(),
                        purchase_id.as_deref(),
                        terminal_id.as_deref(),
                        additional.as_deref(),
                        reference.as_deref(),
                    )
                    .await
                    .map_err(map_client_err)
                    .and_then(to_json)
            }
            "posStarterPurchaseInit" => client
                .pos_starter_purchase_init()
                .await
                .map_err(map_client_err)
                .and_then(to_json),
            "posStarterPurchaseFin" => {
                let main_amount = req_u64(&m, "mainAmount")?;
                let amounts = opt_str(&m, "amounts")?;
                let segment = opt_str(&m, "segment")?;
                let purchase_id = opt_str(&m, "purchaseId")?;
                let terminal_id = opt_str(&m, "terminalId")?;
                let additional = opt_print_data(&m, "additionalData")?;
                let reference = opt_str(&m, "referenceData")?;
                client
                    .pos_starter_purchase_fin(
                        main_amount,
                        amounts.as_deref(),
                        segment.as_deref(),
                        purchase_id.as_deref(),
                        terminal_id.as_deref(),
                        additional.as_deref(),
                        reference.as_deref(),
                    )
                    .await
                    .map_err(map_client_err)
                    .and_then(to_json)
            }
            "posStarterPurchaseCancel" => {
                client
                    .pos_starter_purchase_cancel()
                    .await
                    .map_err(map_client_err)?;
                Ok(json!({ "ok": true }))
            }
            "billPayment" => {
                let bill_id = req_str(&m, "billId")?;
                let payment_id = req_str(&m, "paymentId")?;
                let additional = opt_print_data(&m, "additionalData")?;
                let reference = opt_str(&m, "referenceData")?;
                client
                    .bill_payment(&bill_id, &payment_id, additional.as_deref(), reference.as_deref())
                    .await
                    .map_err(map_client_err)
                    .and_then(to_json)
            }
            "billRequest" => client.bill_request().await.map_err(map_client_err).and_then(to_json),
            "pinCharge" => {
                let additional = opt_print_data(&m, "additionalData")?;
                let reference = opt_str(&m, "referenceData")?;
                client
                    .pin_charge(additional.as_deref(), reference.as_deref())
                    .await
                    .map_err(map_client_err)
                    .and_then(to_json)
            }
            "topupCharge" => {
                let mobile = req_str(&m, "mobileNumber")?;
                let additional = opt_print_data(&m, "additionalData")?;
                let reference = opt_str(&m, "referenceData")?;
                client
                    .topup_charge(&mobile, additional.as_deref(), reference.as_deref())
                    .await
                    .map_err(map_client_err)
                    .and_then(to_json)
            }
            "mciBillInquiry" => {
                let number = req_str(&m, "mciNumber")?;
                let bill_type = req_i64(&m, "billType")?;
                client
                    .mci_bill_inquiry(&number, bill_type)
                    .await
                    .map_err(map_client_err)
                    .and_then(to_json)
            }
            "tciBillInquiry" => {
                let number = req_str(&m, "tciNumber")?;
                let bill_type = req_i64(&m, "billType")?;
                client
                    .tci_bill_inquiry(&number, bill_type)
                    .await
                    .map_err(map_client_err)
                    .and_then(to_json)
            }
            "totalReport" => {
                let from = req_str(&m, "fromDate")?;
                let to = req_str(&m, "toDate")?;
                let pin = opt_str(&m, "posPin")?;
                client
                    .total_report(&from, &to, pin.as_deref())
                    .await
                    .map_err(map_client_err)
                    .and_then(to_json)
            }
            "report" => {
                let filter = req_i64(&m, "filter")?;
                let values = req_str_vec(&m, "filterValues")?;
                let report_type = req_i64(&m, "reportType")?;
                let pin = opt_str(&m, "posPin")?;
                client
                    .report(filter, &values, report_type, pin.as_deref())
                    .await
                    .map_err(map_client_err)
                    .and_then(to_json)
            }
            other => Err(ProviderError::UnknownFunction(other.to_string())),
        }
    }
}

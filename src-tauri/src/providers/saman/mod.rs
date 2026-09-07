//! Saman SSP1126 provider — exposes the full `saman-payment-pos` SDK surface
//! through the generic [`Provider`](super::Provider) interface.

pub mod bill;
pub mod client;
pub mod iso8583;
pub mod response_codes;
pub mod transport;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use self::client::{config_problem, PrintData, SamanClient, SamanConfig};
use super::{
    terminal_function_specs, FunctionSpec, LinkState, LinkStatus, Provider, ProviderError,
    ProviderMetadata, ProviderStatus,
};

pub const PROVIDER_ID: &str = "saman-ssp1126";

/// The SSP1126 keeps its PC link shut from power-on until its payment application
/// has been woken by a card action; from then on the listener stays open. Every
/// place that reports an unreachable terminal says this, because the bare errno
/// ("Connection refused") sends operators hunting for a network fault that is not
/// there.
const PC_LINK_HINT: &str = "After powering the terminal on, run one card action on it \
     (you may cancel it) to start its payment application — the terminal keeps its PC link \
     closed until then, and open afterwards.";

/// Longest a status-light probe may block. The configured connect timeout can be
/// tens of seconds, which is right for a transaction and wrong for a UI poll.
const LINK_PROBE_CAP: Duration = Duration::from_secs(2);

async fn probe_tcp(host: &str, port: u16, timeout: Duration) -> LinkStatus {
    let addr = format!("{host}:{port}");
    let started = std::time::Instant::now();
    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&addr)).await {
        Ok(Ok(_stream)) => LinkStatus::new(LinkState::Up, format!("{addr} accepted a connection"))
            .with_latency(started.elapsed().as_millis() as u64),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            LinkStatus::new(LinkState::Down, format!("{addr} refused the connection"))
                .with_hint(PC_LINK_HINT)
        }
        Ok(Err(e)) => LinkStatus::new(LinkState::Down, format!("{addr}: {e}")),
        Err(_) => LinkStatus::new(
            LinkState::Down,
            format!("{addr} did not answer within {}ms", timeout.as_millis()),
        )
        .with_hint("Check that the terminal is powered on and reachable on this network."),
    }
}

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
    use self::transport::TransportError;
    match e {
        client::ClientError::Config(msg) => ProviderError::NotConfigured(msg),
        // A refused connection means the terminal answered on the network but has
        // no listener. On the SSP1126 that is normal until its payment application
        // has been woken: the PC link stays shut from power-on until the first card
        // action, then stays open. Say so, because the raw errno does not.
        client::ClientError::Transport(TransportError::ConnectionRefused { addr }) => {
            ProviderError::Unreachable(format!(
                "terminal at {addr} refused the connection. It is reachable on the network but has \
                 not opened its PC link. {PC_LINK_HINT}"
            ))
        }
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
                "retryOnFirstTimeout": { "type": "boolean", "default": true, "description": "Re-send the opening request when the terminal does not answer it. Turn off to send exactly once." },
                "firstAckAttempts": { "type": "integer", "default": 3, "minimum": 1, "maximum": 10, "description": "How many times to send the opening request, including the first. The terminal drops this message on a fresh connection often enough that one attempt is not enough; waiting longer does not help, re-sending does." },
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
        // Replacing the client drops the open channel and, with it, the record of
        // when the last transaction closed — the terminal needs a measured gap
        // before the next connection. `PUT /api/v1/settings` re-applies every
        // provider config on every save, so without this guard an unrelated
        // settings change would silently break the next transaction.
        if *client.config() == cfg {
            return Ok(());
        }
        *client = SamanClient::new(cfg);
        Ok(())
    }

    async fn probe_link(&self) -> LinkStatus {
        // Never open a second socket to a terminal that is mid-transaction.
        let Ok(client) = self.client.try_lock() else {
            return LinkStatus::new(
                LinkState::Unknown,
                "a transaction is in progress; the terminal was left alone",
            );
        };
        if client.is_connected() {
            return LinkStatus::new(LinkState::Up, "a session is currently open on the terminal");
        }
        if let Some(problem) = config_problem(client.config()) {
            return LinkStatus::new(LinkState::Unknown, problem);
        }
        let cfg = client.config().clone();
        drop(client);

        if cfg.transport == "tcp" {
            let timeout = LINK_PROBE_CAP.min(Duration::from_millis(cfg.connect_timeout_ms));
            probe_tcp(&cfg.host, cfg.port, timeout).await
        } else {
            // Opening the serial device just to look would seize it from the
            // terminal conversation; a real transaction is the only honest probe.
            LinkStatus::new(
                LinkState::Unknown,
                "serial links are not probed — opening the port could disturb the terminal",
            )
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use self::client::ClientError;
    use self::transport::TransportError;

    /// A refused connection is a reachability problem, not an execution failure,
    /// and the SSP1126 only opens its listener once its payment application has
    /// been woken by a card action — so the message has to say that.
    #[test]
    fn refused_connection_becomes_an_unreachable_error_with_an_operator_hint() {
        let err = map_client_err(ClientError::Transport(TransportError::ConnectionRefused {
            addr: "192.168.1.198:1197".into(),
        }));

        let ProviderError::Unreachable(msg) = &err else {
            panic!("expected Unreachable, got {err:?}");
        };
        assert!(msg.contains("192.168.1.198:1197"), "must name the address: {msg}");
        assert!(msg.contains("card"), "must tell the operator to run a card action: {msg}");
    }

    /// Anything else keeps behaving as it does today.
    #[test]
    fn other_transport_errors_stay_execution_failures() {
        let err = map_client_err(ClientError::Transport(TransportError::Timeout(3000)));
        assert!(matches!(err, ProviderError::Execution(_)), "got {err:?}");
    }

    // -------------------------------------------------- configuration churn

    fn full_cfg(port: u16, gap_ms: u64) -> Value {
        serde_json::to_value(client::SamanConfig {
            host: "127.0.0.1".into(),
            port,
            first_ack_timeout_ms: 250,
            retry_delay_ms: 10,
            min_reconnect_gap_ms: gap_ms,
            reconnect_gap_after_partial_ms: gap_ms,
            include_trace: true,
            ..Default::default()
        })
        .unwrap()
    }

    async fn trace_of(provider: &SamanProvider) -> Vec<String> {
        let v = provider.invoke("connectionTest", json!({})).await.expect("no transport error");
        v.get("trace")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    /// The terminal needs a measured gap between connections. Re-applying a
    /// configuration that has not actually changed must not throw that timing
    /// away — `PUT /api/v1/settings` does exactly that on every save, because the
    /// stored config is compared against a normalised one and never matches.
    #[tokio::test]
    async fn reapplying_an_identical_config_keeps_the_reconnect_timing() {
        let port = client::tests_support::spawn_fake_terminal(&[]).await;
        let cfg = full_cfg(port, 400);
        let provider = SamanProvider::new(cfg.clone());

        // A completed transaction leaves the timing the next one must respect.
        let first = trace_of(&provider).await;
        assert!(!first.is_empty(), "tracing should be on");

        provider.set_config(cfg.clone()).await.expect("identical config re-applied");

        let second = trace_of(&provider).await;
        assert!(
            second.iter().any(|l| l.contains("waiting")),
            "the reconnect gap was forgotten after a no-op config write; trace: {second:?}"
        );
    }

    /// Control: without the config write, the gap is honoured.
    #[tokio::test]
    async fn back_to_back_transactions_honour_the_reconnect_gap() {
        let port = client::tests_support::spawn_fake_terminal(&[]).await;
        let provider = SamanProvider::new(full_cfg(port, 400));

        trace_of(&provider).await;
        let second = trace_of(&provider).await;

        assert!(
            second.iter().any(|l| l.contains("waiting")),
            "trace: {second:?}"
        );
    }

    // ------------------------------------------------------------ link probe

    fn tcp_provider(port: u16) -> SamanProvider {
        SamanProvider::new(json!({ "transport": "tcp", "host": "127.0.0.1", "port": port }))
    }

    #[tokio::test]
    async fn link_probe_reports_up_when_the_terminal_accepts_connections() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let status = tcp_provider(port).probe_link().await;

        assert_eq!(status.state, LinkState::Up, "detail: {}", status.detail);
        assert!(status.latency_ms.is_some(), "an up link should be timed");
    }

    #[tokio::test]
    async fn link_probe_reports_down_with_the_card_hint_when_refused() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let status = tcp_provider(port).probe_link().await;

        assert_eq!(status.state, LinkState::Down, "detail: {}", status.detail);
        let hint = status.hint.expect("a refused link must explain what to do");
        assert!(hint.contains("card"), "hint should mention the card action: {hint}");
    }

    /// Nothing to probe until someone fills in the address.
    #[tokio::test]
    async fn link_probe_is_unknown_when_the_provider_is_not_configured() {
        let status = SamanProvider::new(Value::Null).probe_link().await;
        assert_eq!(status.state, LinkState::Unknown, "detail: {}", status.detail);
    }

    /// The probe opens a real socket to a payment terminal, so it must never run
    /// while a transaction owns the client.
    #[tokio::test]
    async fn link_probe_does_not_touch_the_terminal_while_busy() {
        // Point at a closed port: if the probe ran, it would report Down.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let provider = tcp_provider(port);
        let _busy = provider.client.lock().await;

        let status = provider.probe_link().await;

        assert_eq!(status.state, LinkState::Unknown, "detail: {}", status.detail);
    }
}

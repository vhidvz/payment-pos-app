//! High-level client for the SSP1126 POS terminal — a faithful Rust port of
//! `Ssp1126Client` from the `saman-payment-pos` npm library.
//!
//! Each public transaction method opens a connection (if needed), runs the
//! multi-step ISO-8583 conversation, sends the closing Dispose message and
//! closes the channel — mirroring the reference Delphi component's blocking
//! `Do*` methods, including its measured reconnect-gap and first-ack retry
//! behavior (see the option docs below).

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use super::iso8583::{verify_mac, Iso8583Message};
use super::response_codes::response_message;
use super::transport::{SerialTransport, TcpTransport, Transport, TransportError};

const MTI: &str = "0300";
const POS_CONDITION: &str = "14";

// ------------------------------------------------------------------- config

fn d_transport() -> String { "tcp".into() }
fn d_port() -> u16 { 1197 }
fn d_baud() -> u32 { 19200 }
fn d_connect_timeout() -> u64 { 10_000 }
fn d_currency() -> String { "364".into() }
fn d_component_version() -> String { "1.4.3.0".into() }
fn d_true() -> bool { true }
fn d_min_gap() -> u64 { 500 }
fn d_partial_gap() -> u64 { 1500 }
fn d_first_ack() -> u64 { 3000 }
fn d_retry_delay() -> u64 { 500 }
fn d_first_ack_attempts() -> u32 { 3 }

/// Saman SSP1126 provider configuration. Field semantics and defaults mirror
/// `Ssp1126ClientOptions` in the reference library one-for-one.
/// Unknown fields are rejected so a typo'd key can't be silently ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct SamanConfig {
    /// Channel: "tcp" or "serial".
    pub transport: String,
    /// Terminal IP (TCP).
    pub host: String,
    /// Terminal TCP port (TCP). Default 1197.
    pub port: u16,
    /// Serial device, e.g. "COM3" / "/dev/ttyUSB0" (serial).
    pub path: String,
    /// Serial baud rate. Default 19200 (8-N-1 fixed).
    pub baud_rate: u32,
    /// Connect / open timeout in ms. Default 10000.
    pub connect_timeout_ms: u64,
    /// DE49 currency. Default "364" (IRR).
    pub currency: String,
    /// DE57 component version for purchases. Default "1.4.3.0".
    pub component_version: String,
    /// Verify the DE64 MAC on incoming messages. Default true.
    pub verify_mac: bool,
    /// Reconnect gap after a transaction that ended cleanly (final `17`). Default 500.
    pub min_reconnect_gap_ms: u64,
    /// Reconnect gap after a transaction without a final `17`. Default 1500.
    pub reconnect_gap_after_partial_ms: u64,
    /// Timeout for the opening `15` acknowledgement only. Default 3000.
    pub first_ack_timeout_ms: u64,
    /// Master switch for re-sending an unanswered opening request. Default true.
    /// Kept so existing settings files keep loading; set it false to send once.
    pub retry_on_first_timeout: bool,
    /// How many times to send the opening request before giving up, including the
    /// first. Default 3.
    ///
    /// The SSP1126 *drops* the opening message on a fresh connection rather than
    /// answering it slowly — a 20-second wait produced nothing where a re-send
    /// answered in 3 — so this, not `first_ack_timeout_ms`, is the lever that
    /// makes a transaction land.
    pub first_ack_attempts: u32,
    /// Delay before that retry. Default 500.
    pub retry_delay_ms: u64,
    /// Attach the raw hex frame trace to every result (debugging aid). Default false.
    pub include_trace: bool,
}

impl Default for SamanConfig {
    fn default() -> Self {
        Self {
            transport: d_transport(),
            host: String::new(),
            port: d_port(),
            path: String::new(),
            baud_rate: d_baud(),
            connect_timeout_ms: d_connect_timeout(),
            currency: d_currency(),
            component_version: d_component_version(),
            verify_mac: d_true(),
            min_reconnect_gap_ms: d_min_gap(),
            reconnect_gap_after_partial_ms: d_partial_gap(),
            first_ack_timeout_ms: d_first_ack(),
            retry_on_first_timeout: d_true(),
            first_ack_attempts: d_first_ack_attempts(),
            retry_delay_ms: d_retry_delay(),
            include_trace: false,
        }
    }
}

// -------------------------------------------------------------------- types

/// One extra line to print on the receipt (packed into DE48).
/// alignment: 0=Right 1=Left 2=Center; receipt: 0=Customer 1=Merchant 2=Both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintData {
    pub alignment: u8,
    pub receipt: u8,
    pub title: String,
    pub value: String,
}

/// Normalised result of a transaction. Mirrors the reference `PcPosResults`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_id: Option<String>,
    /// Retrieval reference number (DE37).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_amount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_mask: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_hash1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_hash2: Option<String>,
    /// Charge PIN / serial / emergency number (DE58/59/60) for charge transactions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge_pin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge_serial: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge_emergency_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos_version: Option<String>,
    /// All raw ISO fields harvested across the conversation, keyed by DE number.
    pub fields: BTreeMap<u16, String>,
    /// True if the transaction failed because an expected step never arrived within
    /// its timeout, as opposed to a decline / bad MAC. Only meaningful when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
    /// Which processing code actually answered a diagnostic probe. Only set by
    /// `connection_test`, which has more than one way to reach the terminal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<String>,
    /// Raw POS messages exchanged (hex), when `includeTrace` is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<Vec<String>>,
}

/// One payment routing option offered after a card read in a POS-started purchase.
#[derive(Debug, Clone, Serialize)]
pub struct CardSegment {
    pub code: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PosStarterPurchaseInitResult {
    #[serde(flatten)]
    pub result: TransactionResult,
    /// Routing options read off the card; usually a single entry.
    pub segments: Vec<CardSegment>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizedOperations {
    pub balance: bool,
    pub bill: bool,
    pub report: bool,
    pub mci_bill: bool,
    pub pin_charge: bool,
    pub purchase: bool,
    pub topup_charge: bool,
    pub tci_bill: bool,
    pub payment_service: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos_version: Option<String>,
    pub result: TransactionResult,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TotalReportResult {
    pub bills_count: u64,
    pub bills_total_amount: u64,
    pub pin_charge_count: u64,
    pub pin_charge_amount: u64,
    pub topup_charge_count: u64,
    pub topup_charge_amount: u64,
    pub purchase_count: u64,
    pub purchase_amount: u64,
    pub group_charge_count: u64,
    pub group_charge_amount: u64,
    pub result: TransactionResult,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRow {
    pub date: String,
    pub shift_code: String,
    pub trace_number: String,
    pub amount: u64,
    pub rrn: String,
    pub bank: String,
    pub card_mask: String,
    pub card_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bill_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_id: Option<String>,
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportResult {
    pub rows: Vec<ReportRow>,
    pub result: TransactionResult,
}

/// 0=Purchase 1=Bill 2=PinCharge 3=TopupCharge 6=PaymentService
pub type ReportType = i64;
/// 1=Date 2=Serial 3=Time
pub type ReportFilter = i64;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{0}")]
    Transport(#[from] TransportError),
    #[error("iso8583: {0}")]
    Iso(#[from] super::iso8583::IsoError),
    #[error("{0}")]
    Config(String),
}

// ----------------------------------------------------------- harvest targets

#[derive(Clone, Copy)]
enum Target {
    ResponseCode,
    TerminalId,
    TraceNumber,
    SerialId,
    Rrn,
    Amount,
    EffectiveAmount,
    TransactionDate,
    CardMask,
    CardHash1,
    CardHash2,
    ChargePin,
    ChargeSerial,
    ChargeEmergencyNumber,
    PosVersion,
}

fn assign(result: &mut TransactionResult, target: Target, v: &str) {
    let v = Some(v.to_string());
    match target {
        Target::ResponseCode => result.response_code = v,
        Target::TerminalId => result.terminal_id = v,
        Target::TraceNumber => result.trace_number = v,
        Target::SerialId => result.serial_id = v,
        Target::Rrn => result.rrn = v,
        Target::Amount => result.amount = v,
        Target::EffectiveAmount => result.effective_amount = v,
        Target::TransactionDate => result.transaction_date = v,
        Target::CardMask => result.card_mask = v,
        Target::CardHash1 => result.card_hash1 = v,
        Target::CardHash2 => result.card_hash2 = v,
        Target::ChargePin => result.charge_pin = v,
        Target::ChargeSerial => result.charge_serial = v,
        Target::ChargeEmergencyNumber => result.charge_emergency_number = v,
        Target::PosVersion => result.pos_version = v,
    }
}

type HarvestMap<'a> = &'a [(u16, Option<Target>)];

const FINAL_FIELDS_COMMON: HarvestMap<'static> = &[
    (6, Some(Target::Amount)),
    (11, Some(Target::SerialId)),
    (37, Some(Target::Rrn)),
    (38, Some(Target::TraceNumber)),
    (39, Some(Target::ResponseCode)),
    (46, Some(Target::TransactionDate)),
    (54, Some(Target::EffectiveAmount)),
];

// ------------------------------------------------------------------- client

struct Recv {
    iso: Iso8583Message,
    mac_ok: bool,
}

pub struct SamanClient {
    cfg: SamanConfig,
    transport: Box<dyn Transport>,
    terminal_id: String,
    last_closed_at: Option<Instant>,
    just_reconnected: bool,
    /// Did the current transaction receive the terminal's final `17`?
    saw_eot: bool,
    /// Same, for the most recently closed transaction — drives the reconnect gap.
    last_txn_ended_with_eot: bool,
    log: Vec<String>,
}

/// Placeholder transport for a not-yet-connected (or not-yet-configured) client.
struct NullTransport;

#[async_trait::async_trait]
impl Transport for NullTransport {
    async fn connect(&mut self) -> Result<(), TransportError> {
        Err(TransportError::NotConnected)
    }
    fn connected(&self) -> bool {
        false
    }
    async fn send(&mut self, _: &[u8]) -> Result<(), TransportError> {
        Err(TransportError::NotConnected)
    }
    async fn receive(&mut self, _: Duration) -> Result<Vec<u8>, TransportError> {
        Err(TransportError::NotConnected)
    }
    fn clear_inbox(&mut self) {}
    fn close(&mut self) {}
}

/// Returns why this config cannot open a connection yet, if anything.
pub fn config_problem(cfg: &SamanConfig) -> Option<String> {
    match cfg.transport.as_str() {
        "serial" => cfg
            .path
            .is_empty()
            .then(|| "serial transport requires a device path (e.g. COM3 or /dev/ttyUSB0)".to_string()),
        "tcp" => cfg
            .host
            .is_empty()
            .then(|| "tcp transport requires the terminal host (IP address)".to_string()),
        other => Some(format!("unknown transport '{other}' (expected \"tcp\" or \"serial\")")),
    }
}

fn make_transport(cfg: &SamanConfig) -> Result<Box<dyn Transport>, ClientError> {
    if let Some(problem) = config_problem(cfg) {
        return Err(ClientError::Config(problem));
    }
    let timeout = Duration::from_millis(cfg.connect_timeout_ms);
    match cfg.transport.as_str() {
        "serial" => Ok(Box::new(SerialTransport::new(&cfg.path, cfg.baud_rate, timeout))),
        _ => Ok(Box::new(TcpTransport::new(&cfg.host, cfg.port, timeout))),
    }
}

impl SamanClient {
    pub fn new(cfg: SamanConfig) -> Self {
        Self {
            cfg,
            transport: Box::new(NullTransport),
            terminal_id: String::new(),
            last_closed_at: None,
            just_reconnected: false,
            saw_eot: false,
            last_txn_ended_with_eot: false,
            log: Vec::new(),
        }
    }

    pub fn config(&self) -> &SamanConfig {
        &self.cfg
    }

    pub fn is_connected(&self) -> bool {
        self.transport.connected()
    }

    // ------------------------------------------------------------- low level

    fn trace_line(&mut self, line: String) {
        tracing::debug!(target: "saman", "{line}");
        if self.cfg.include_trace {
            self.log.push(line);
        }
    }

    async fn ensure_connected(&mut self) -> Result<(), ClientError> {
        if !self.transport.connected() {
            // How long the terminal needs depends on how the last transaction ended:
            // quickly after a clean `17`, substantially longer after a partial flow.
            let gap = Duration::from_millis(if self.last_txn_ended_with_eot {
                self.cfg.min_reconnect_gap_ms
            } else {
                self.cfg.reconnect_gap_after_partial_ms
            });
            if !gap.is_zero() {
                if let Some(at) = self.last_closed_at {
                    let elapsed = at.elapsed();
                    if elapsed < gap {
                        let wait = gap - elapsed;
                        self.trace_line(format!("  waiting {}ms before reconnecting", wait.as_millis()));
                        tokio::time::sleep(wait).await;
                    }
                }
            }
            self.transport = make_transport(&self.cfg)?;
            self.transport.connect().await?;
            self.just_reconnected = true;
        } else {
            self.just_reconnected = false;
        }
        Ok(())
    }

    fn now_fields() -> (String, String) {
        let now = chrono::Local::now();
        (now.format("%H%M%S").to_string(), now.format("%m%d").to_string())
    }

    async fn send(&mut self, msg: &mut Iso8583Message) -> Result<(), ClientError> {
        let raw = msg.pack_with_mac()?;
        self.trace_line(format!("> {}", hex(&raw)));
        self.transport.clear_inbox();
        self.transport.send(&raw).await?;
        Ok(())
    }

    async fn recv(&mut self, timeout_ms: u64) -> Result<Option<Recv>, ClientError> {
        match self.transport.receive(Duration::from_millis(timeout_ms)).await {
            Ok(raw) => {
                self.trace_line(format!("< {}", hex(&raw)));
                let iso = Iso8583Message::unpack(&raw)?;
                let mac_ok = if self.cfg.verify_mac { verify_mac(&raw) } else { true };
                Ok(Some(Recv { iso, mac_ok }))
            }
            Err(TransportError::Timeout(ms)) => {
                self.trace_line(format!("< (timeout after {ms}ms, no frame received)"));
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Send the opening request of a transaction and wait for the first response.
    ///
    /// The terminal drops this message outright on a freshly opened connection a
    /// good fraction of the time — measured on an SSP1126, waiting twenty seconds
    /// produced nothing where re-sending was answered in three. So an unanswered
    /// opening request is re-sent, up to `first_ack_attempts` times, each on a
    /// fresh connection. Only a *fresh* connection qualifies: a terminal that
    /// falls silent part-way through a conversation is a different fault and must
    /// not be handed the same request again.
    async fn send_and_await_first_ack(
        &mut self,
        req: &mut Iso8583Message,
    ) -> Result<Option<Recv>, ClientError> {
        let timeout_ms = self.cfg.first_ack_timeout_ms;
        let attempts = if self.cfg.retry_on_first_timeout {
            self.cfg.first_ack_attempts.max(1)
        } else {
            1
        };

        for attempt in 1..=attempts {
            let on_fresh_connection = self.just_reconnected;
            self.send(req).await?;
            if let Some(r) = self.recv(timeout_ms).await? {
                return Ok(Some(r));
            }
            if attempt == attempts || !on_fresh_connection {
                break;
            }
            let delay = self.cfg.retry_delay_ms;
            self.trace_line(format!(
                "  opening request unanswered ({attempt}/{attempts}); re-sending after {delay}ms"
            ));
            self.transport.close();
            self.last_closed_at = Some(Instant::now());
            tokio::time::sleep(Duration::from_millis(delay)).await;
            self.ensure_connected().await?;
        }
        Ok(None)
    }

    fn validate(r: &Option<Recv>, proc: Option<&str>, rc: Option<&str>) -> bool {
        let Some(r) = r else { return false };
        if !r.mac_ok {
            return false;
        }
        if let Some(p) = proc {
            if r.iso.get_str(3).as_deref() != Some(p) {
                return false;
            }
        }
        if let Some(c) = rc {
            if r.iso.get_str(39).as_deref() != Some(c) {
                return false;
            }
        }
        true
    }

    fn base_message(&self, proc: &str) -> Iso8583Message {
        let (time, date) = Self::now_fields();
        let mut m = Iso8583Message::new();
        m.set_mti(MTI)
            .set_str(3, proc)
            .set_str(12, &time)
            .set_str(13, &date)
            .set_str(25, POS_CONDITION);
        m
    }

    async fn send_ack(&mut self) -> Result<(), ClientError> {
        let (time, date) = Self::now_fields();
        let mut ack = Iso8583Message::new();
        ack.set_mti(MTI)
            .set_str(3, "000004")
            .set_str(12, &time)
            .set_str(13, &date)
            .set_str(25, POS_CONDITION);
        if !self.terminal_id.is_empty() {
            let tid = self.terminal_id.clone();
            ack.set_str(41, &tid);
        }
        self.send(&mut ack).await
    }

    async fn send_dispose(&mut self) -> Result<(), ClientError> {
        let (time, date) = Self::now_fields();
        let mut d = Iso8583Message::new();
        d.set_mti(MTI)
            .set_str(3, "000001")
            .set_str(12, &time)
            .set_str(13, &date)
            .set_str(25, POS_CONDITION);
        self.send(&mut d).await
    }

    /// Cleanly end a transaction: dispose + close (best-effort).
    async fn finish(&mut self) {
        let _ = self.send_dispose().await;
        self.transport.close();
        self.last_closed_at = Some(Instant::now());
        self.last_txn_ended_with_eot = self.saw_eot;
    }

    /// Pack extra receipt lines into DE48: `<count><align><receipt><LLL title><LLL value>…`,
    /// at most 9 items and ~350 characters of payload. `|` is replaced with a space
    /// (it is a field separator on newer terminal firmware).
    fn pack_print_data(items: Option<&[PrintData]>) -> Option<String> {
        let items = items?;
        if items.is_empty() {
            return None;
        }
        let clean = |s: &str| s.replace('|', " ");
        let mut payload = String::new();
        let mut count = 0usize;
        let mut total = 0usize;
        for it in items.iter().take(9) {
            let title = clean(&it.title);
            let value = clean(&it.value);
            total += 8 + title.chars().count() + value.chars().count();
            if total > 350 {
                break;
            }
            payload.push_str(&format!(
                "{}{}{:03}{}{:03}{}",
                it.alignment,
                it.receipt,
                title.chars().count(),
                title,
                value.chars().count(),
                value
            ));
            count += 1;
        }
        if count == 0 {
            return None;
        }
        Some(format!("{count}{payload}"))
    }

    fn harvest(&mut self, result: &mut TransactionResult, iso: &Iso8583Message, map: HarvestMap) {
        for &(de, target) in map {
            let Some(v) = iso.get_str(de) else { continue };
            result.fields.insert(de, v.clone());
            if let Some(t) = target {
                assign(result, t, &v);
            }
            if de == 41 && !v.is_empty() {
                self.terminal_id = v;
            }
        }
    }

    fn finalize_failure(&mut self, result: &mut TransactionResult, last: &Option<Recv>) {
        // `last` is None only when the most recent recv timed out (no frame at all).
        result.timed_out = Some(last.is_none());
        if let Some(l) = last {
            if l.mac_ok {
                let iso = l.iso.clone();
                self.harvest(
                    result,
                    &iso,
                    &[(39, Some(Target::ResponseCode)), (41, Some(Target::TerminalId))],
                );
            }
        }
        result.response_message = result
            .response_code
            .as_deref()
            .and_then(response_message)
            .map(String::from);
    }

    fn set_success_message(result: &mut TransactionResult) {
        result.response_message = result
            .response_code
            .as_deref()
            .and_then(response_message)
            .map(String::from);
    }

    fn begin_txn(&mut self) {
        self.saw_eot = false;
        self.log.clear();
    }

    fn attach_trace(&mut self, result: &mut TransactionResult) {
        if self.cfg.include_trace {
            result.trace = Some(std::mem::take(&mut self.log));
        }
    }

    // --------------------------------------------------------- public surface

    /// Verify connectivity with the terminal (processing code 410000).
    ///
    /// A terminal can stay silent to 410000 while the link is perfectly healthy —
    /// observed on an SSP1126 (firmware 10.063.00PO) in the minutes after its PC
    /// link first opened, where 410000 timed out twice in a row and 390000 answered
    /// normally on the same socket, and where 410000 later answered fine. Since this
    /// is the check an operator reaches for when something looks wrong, silence is
    /// retried as the authorized-operations probe rather than reported as a dead
    /// terminal. `probe` in the result says which one answered.
    pub async fn connection_test(&mut self) -> Result<TransactionResult, ClientError> {
        self.begin_txn();
        let mut result = TransactionResult::default();
        let mut last: Option<Recv> = None;
        let mut err = self.connection_test_inner(&mut result, &mut last).await.err();

        if result.ok {
            result.probe = Some("410000".into());
        } else if err.is_none() && last.is_none() {
            // No frame came back at all — silence, not a decline. Try the other probe.
            self.finish().await;
            self.trace_line("  410000 unanswered; re-checking with 390000".into());
            result = TransactionResult::default();
            last = None;
            err = self
                .authorized_operations_inner(&mut result, &mut last)
                .await
                .map(|_flags| ())
                .err();
            if result.ok {
                result.probe = Some("390000".into());
            }
        }

        if !result.ok {
            self.finalize_failure(&mut result, &last);
        } else {
            Self::set_success_message(&mut result);
        }
        self.finish().await;
        self.attach_trace(&mut result);
        match err {
            Some(e) => Err(e),
            None => Ok(result),
        }
    }

    async fn connection_test_inner(
        &mut self,
        result: &mut TransactionResult,
        last: &mut Option<Recv>,
    ) -> Result<(), ClientError> {
        self.ensure_connected().await?;
        let mut req = self.base_message("410000");
        let currency = self.cfg.currency.clone();
        req.set_str(49, &currency);
        *last = self.send_and_await_first_ack(&mut req).await?;
        if Self::validate(last, None, Some("15")) {
            let iso = last.as_ref().unwrap().iso.clone();
            self.harvest(result, &iso, &[(12, None), (13, None), (41, Some(Target::TerminalId))]);
            *last = self.recv(10_000).await?;
            if Self::validate(last, Some("410003"), None) {
                let iso = last.as_ref().unwrap().iso.clone();
                self.harvest(result, &iso, &[(39, Some(Target::ResponseCode))]);
                self.send_ack().await?;
                *last = self.recv(10_000).await?;
                if Self::validate(last, None, Some("17")) {
                    result.ok = true;
                    self.saw_eot = true;
                }
            }
        }
        Ok(())
    }

    /// Account balance inquiry (processing code 310000).
    pub async fn balance(&mut self) -> Result<TransactionResult, ClientError> {
        let currency = self.cfg.currency.clone();
        self.card_financial_flow(
            "310000",
            "310003",
            None,
            move |m: &mut Iso8583Message| {
                m.set_str(49, &currency);
            },
            &[
                (11, Some(Target::SerialId)),
                (37, Some(Target::Rrn)),
                (38, Some(Target::TraceNumber)),
                (39, Some(Target::ResponseCode)),
                (46, Some(Target::TransactionDate)),
            ],
        )
        .await
    }

    /// Bill payment (processing code 400000).
    pub async fn bill_payment(
        &mut self,
        bill_id: &str,
        payment_id: &str,
        additional_data: Option<&[PrintData]>,
        reference_data: Option<&str>,
    ) -> Result<TransactionResult, ClientError> {
        let currency = self.cfg.currency.clone();
        let print = Self::pack_print_data(additional_data);
        let bill_id = bill_id.to_string();
        let payment_id = payment_id.to_string();
        let reference = reference_data.map(String::from);
        self.card_financial_flow(
            "400000",
            "400003",
            None,
            move |m: &mut Iso8583Message| {
                m.set_str(46, "200");
                if let Some(p) = &print {
                    m.set_str(48, p);
                }
                m.set_str(49, &currency);
                if let Some(r) = &reference {
                    m.set_str(56, r);
                }
                m.set_str(60, &bill_id);
                m.set_str(61, &payment_id);
            },
            FINAL_FIELDS_COMMON,
        )
        .await
    }

    /// Bill request — reads bill data from the inserted card (400000, DE46=300).
    pub async fn bill_request(&mut self) -> Result<TransactionResult, ClientError> {
        let currency = self.cfg.currency.clone();
        self.card_financial_flow(
            "400000",
            "400003",
            None,
            move |m: &mut Iso8583Message| {
                m.set_str(46, "300");
                m.set_str(49, &currency);
            },
            FINAL_FIELDS_COMMON,
        )
        .await
    }

    /// PIN charge (processing code 300000).
    pub async fn pin_charge(
        &mut self,
        additional_data: Option<&[PrintData]>,
        reference_data: Option<&str>,
    ) -> Result<TransactionResult, ClientError> {
        let currency = self.cfg.currency.clone();
        let print = Self::pack_print_data(additional_data);
        let reference = reference_data.map(String::from);
        self.card_financial_flow(
            "300000",
            "300003",
            Some(&[
                (66, Some(Target::CardHash2)),
                (2, Some(Target::CardMask)),
                (63, Some(Target::CardHash1)),
            ]),
            move |m: &mut Iso8583Message| {
                if let Some(p) = &print {
                    m.set_str(48, p);
                }
                m.set_str(49, &currency);
                if let Some(r) = &reference {
                    m.set_str(56, r);
                }
            },
            &[
                (6, Some(Target::Amount)),
                (11, Some(Target::SerialId)),
                (37, Some(Target::Rrn)),
                (38, Some(Target::TraceNumber)),
                (39, Some(Target::ResponseCode)),
                (46, Some(Target::TransactionDate)),
                (54, Some(Target::EffectiveAmount)),
                (58, Some(Target::ChargePin)),
                (59, Some(Target::ChargeSerial)),
                (60, Some(Target::ChargeEmergencyNumber)),
            ],
        )
        .await
    }

    /// Top-up charge for a mobile number (processing code 370000).
    pub async fn topup_charge(
        &mut self,
        mobile_number: &str,
        additional_data: Option<&[PrintData]>,
        reference_data: Option<&str>,
    ) -> Result<TransactionResult, ClientError> {
        let currency = self.cfg.currency.clone();
        let print = Self::pack_print_data(additional_data);
        let mobile = mobile_number.to_string();
        let reference = reference_data.map(String::from);
        self.card_financial_flow(
            "370000",
            "370003",
            None,
            move |m: &mut Iso8583Message| {
                if let Some(p) = &print {
                    m.set_str(48, p);
                }
                m.set_str(49, &currency);
                if let Some(r) = &reference {
                    m.set_str(56, r);
                }
                m.set_str(60, &mobile);
            },
            &[
                (6, Some(Target::Amount)),
                (11, Some(Target::SerialId)),
                (37, Some(Target::Rrn)),
                (38, Some(Target::TraceNumber)),
                (39, Some(Target::ResponseCode)),
                (46, Some(Target::TransactionDate)),
                (54, Some(Target::EffectiveAmount)),
                (58, Some(Target::ChargePin)),
                (59, Some(Target::ChargeSerial)),
                (60, Some(Target::ChargeEmergencyNumber)),
            ],
        )
        .await
    }

    /// MCI (Hamrah-e-Aval) bill inquiry — 170000, DE46=100, number in DE60, type in DE61.
    pub async fn mci_bill_inquiry(
        &mut self,
        mci_number: &str,
        bill_type: i64,
    ) -> Result<TransactionResult, ClientError> {
        self.xci_bill_inquiry("170000", "170003", mci_number, bill_type).await
    }

    /// TCI (telecom) bill inquiry — 180000, DE46=100, number in DE60, type in DE61.
    pub async fn tci_bill_inquiry(
        &mut self,
        tci_number: &str,
        bill_type: i64,
    ) -> Result<TransactionResult, ClientError> {
        self.xci_bill_inquiry("180000", "180003", tci_number, bill_type).await
    }

    async fn xci_bill_inquiry(
        &mut self,
        proc: &str,
        mid_proc: &str,
        number: &str,
        bill_type: i64,
    ) -> Result<TransactionResult, ClientError> {
        let currency = self.cfg.currency.clone();
        let number = number.to_string();
        self.card_financial_flow(
            proc,
            mid_proc,
            None,
            move |m: &mut Iso8583Message| {
                m.set_str(46, "100");
                m.set_str(49, &currency);
                m.set_str(60, &number);
                m.set_str(61, &bill_type.to_string());
            },
            &[
                (11, Some(Target::SerialId)),
                (39, Some(Target::ResponseCode)),
                (46, Some(Target::TransactionDate)),
                (54, Some(Target::EffectiveAmount)),
            ],
        )
        .await
    }

    /// Purchase started by the PC (processing code 000000).
    ///
    /// **`purchase_id` selects a different transaction type — leave it unset for an
    /// ordinary sale.** Populating DE63 requests an *identified purchase*, which is
    /// provisioned separately; without that entitlement the terminal answers `07`.
    #[allow(clippy::too_many_arguments)]
    pub async fn purchase(
        &mut self,
        main_amount: u64,
        amounts: Option<&str>,
        purchase_id: Option<&str>,
        terminal_id: Option<&str>,
        additional_data: Option<&[PrintData]>,
        reference_data: Option<&str>,
    ) -> Result<TransactionResult, ClientError> {
        self.begin_txn();
        let mut result = TransactionResult::default();
        let mut last: Option<Recv> = None;
        let err = self
            .purchase_inner(
                main_amount,
                amounts,
                purchase_id,
                terminal_id,
                additional_data,
                reference_data,
                &mut result,
                &mut last,
            )
            .await
            .err();
        if !result.ok {
            self.finalize_failure(&mut result, &last);
        } else {
            Self::set_success_message(&mut result);
        }
        self.finish().await;
        self.attach_trace(&mut result);
        match err {
            Some(e) => Err(e),
            None => Ok(result),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn purchase_inner(
        &mut self,
        main_amount: u64,
        amounts: Option<&str>,
        purchase_id: Option<&str>,
        terminal_id: Option<&str>,
        additional_data: Option<&[PrintData]>,
        reference_data: Option<&str>,
        result: &mut TransactionResult,
        last: &mut Option<Recv>,
    ) -> Result<(), ClientError> {
        self.ensure_connected().await?;
        let mut m = self.base_message("000000");
        m.set_str(4, &main_amount.to_string());
        if let Some(pid) = purchase_id {
            if pid.len() > 30 {
                m.set_str(24, "001");
                m.set_str(25, "15");
            }
        }
        if let Some(tid) = terminal_id {
            if !tid.is_empty() {
                m.set_str(41, tid);
            }
        }
        m.set_str(46, "300");
        if let Some(a) = amounts {
            m.set_str(47, a);
        }
        if let Some(p) = Self::pack_print_data(additional_data) {
            m.set_str(48, &p);
        }
        let currency = self.cfg.currency.clone();
        m.set_str(49, &currency);
        if let Some(r) = reference_data {
            m.set_str(56, r);
        }
        let cv = self.cfg.component_version.clone();
        m.set_str(57, &cv);
        if let Some(pid) = purchase_id {
            if !pid.is_empty() {
                m.set_str(63, pid);
            }
        }
        *last = self.send_and_await_first_ack(&mut m).await?;
        if Self::validate(last, None, Some("15")) {
            let iso = last.as_ref().unwrap().iso.clone();
            self.harvest(result, &iso, &[(12, None), (13, None), (41, Some(Target::TerminalId))]);
            *last = self.recv(120_000).await?;
            if Self::validate(last, Some("000008"), None) {
                let iso = last.as_ref().unwrap().iso.clone();
                self.harvest(
                    result,
                    &iso,
                    &[
                        (6, Some(Target::Amount)),
                        (11, Some(Target::SerialId)),
                        (37, Some(Target::Rrn)),
                        (38, Some(Target::TraceNumber)),
                        (39, Some(Target::ResponseCode)),
                        (46, Some(Target::TransactionDate)),
                        (54, Some(Target::EffectiveAmount)),
                        (60, Some(Target::CardHash2)),
                        (62, Some(Target::CardMask)),
                        (63, Some(Target::CardHash1)),
                    ],
                );
                self.send_ack().await?;
                *last = self.recv(10_000).await?;
                if Self::validate(last, None, Some("17")) {
                    result.ok = true;
                    self.saw_eot = true;
                }
            }
        }
        Ok(())
    }

    /// Step 1 of a POS-started purchase (000000, DE46=100): the customer taps their
    /// card first; the terminal reports back the payment routing options.
    /// On success the connection is deliberately **left open** for `pos_starter_purchase_fin`.
    pub async fn pos_starter_purchase_init(
        &mut self,
    ) -> Result<PosStarterPurchaseInitResult, ClientError> {
        self.begin_txn();
        let mut result = TransactionResult::default();
        let mut segments: Vec<CardSegment> = Vec::new();
        let mut last: Option<Recv> = None;
        let mut leave_open = false;

        let err = async {
            self.ensure_connected().await?;
            let mut req = self.base_message("000000");
            let currency = self.cfg.currency.clone();
            req.set_str(46, "100").set_str(49, &currency);
            last = self.send_and_await_first_ack(&mut req).await?;
            if Self::validate(&last, None, Some("15")) {
                let iso = last.as_ref().unwrap().iso.clone();
                self.harvest(&mut result, &iso, &[(12, None), (13, None), (41, Some(Target::TerminalId))]);
                last = self.recv(10_000).await?;
                if Self::validate(&last, Some("000003"), None) {
                    let iso = last.as_ref().unwrap().iso.clone();
                    self.harvest(
                        &mut result,
                        &iso,
                        &[
                            (60, Some(Target::CardHash2)),
                            (62, Some(Target::CardMask)),
                            (63, Some(Target::CardHash1)),
                        ],
                    );
                    self.send_ack().await?;
                    // DE57 is `code*label*code*label...`; pair consecutive parts up.
                    let de57 = iso.get_str(57).unwrap_or_default();
                    let parts: Vec<&str> = de57.split('*').filter(|s| !s.is_empty()).collect();
                    for pair in parts.chunks(2) {
                        segments.push(CardSegment {
                            code: pair[0].to_string(),
                            label: pair.get(1).unwrap_or(&"").to_string(),
                        });
                    }
                    result.ok = true;
                    leave_open = true;
                }
            }
            Ok::<(), ClientError>(())
        }
        .await
        .err();

        if !result.ok {
            self.finalize_failure(&mut result, &last);
        } else {
            Self::set_success_message(&mut result);
        }
        if !leave_open {
            self.finish().await;
        }
        self.attach_trace(&mut result);
        match err {
            Some(e) => Err(e),
            None => Ok(PosStarterPurchaseInitResult { result, segments }),
        }
    }

    /// Step 2 of a POS-started purchase (000000, DE46=200). Always disposes and
    /// closes when done, win or lose. `segment` is the `code` of a segment returned
    /// by init — omit when there was nothing to choose between.
    #[allow(clippy::too_many_arguments)]
    pub async fn pos_starter_purchase_fin(
        &mut self,
        main_amount: u64,
        amounts: Option<&str>,
        segment: Option<&str>,
        purchase_id: Option<&str>,
        terminal_id: Option<&str>,
        additional_data: Option<&[PrintData]>,
        reference_data: Option<&str>,
    ) -> Result<TransactionResult, ClientError> {
        self.begin_txn();
        let mut result = TransactionResult::default();
        let mut last: Option<Recv> = None;

        let err = async {
            self.ensure_connected().await?;
            let mut req = self.base_message("000000");
            req.set_str(4, &main_amount.to_string());
            if let Some(tid) = terminal_id {
                if !tid.is_empty() {
                    req.set_str(41, tid);
                }
            }
            req.set_str(46, "200");
            if let Some(a) = amounts {
                req.set_str(47, a);
            }
            if let Some(p) = Self::pack_print_data(additional_data) {
                req.set_str(48, &p);
            }
            let currency = self.cfg.currency.clone();
            req.set_str(49, &currency);
            if let Some(s) = segment {
                if !s.is_empty() {
                    req.set_str(55, s);
                }
            }
            if let Some(r) = reference_data {
                req.set_str(56, r);
            }
            let cv = self.cfg.component_version.clone();
            req.set_str(57, &cv);
            if let Some(pid) = purchase_id {
                if !pid.is_empty() {
                    req.set_str(63, pid);
                }
            }
            last = self.send_and_await_first_ack(&mut req).await?;
            if Self::validate(&last, None, Some("15")) {
                let iso = last.as_ref().unwrap().iso.clone();
                self.harvest(&mut result, &iso, &[(12, None), (13, None), (41, Some(Target::TerminalId))]);
                last = self.recv(120_000).await?;
                if Self::validate(&last, Some("000008"), None) {
                    let iso = last.as_ref().unwrap().iso.clone();
                    self.harvest(
                        &mut result,
                        &iso,
                        &[
                            (6, Some(Target::Amount)),
                            (11, Some(Target::SerialId)),
                            (37, Some(Target::Rrn)),
                            (38, Some(Target::TraceNumber)),
                            (39, Some(Target::ResponseCode)),
                            (46, Some(Target::TransactionDate)),
                            (54, Some(Target::EffectiveAmount)),
                            (62, Some(Target::CardMask)),
                            (63, Some(Target::CardHash1)),
                        ],
                    );
                    self.send_ack().await?;
                    last = self.recv(10_000).await?;
                    if Self::validate(&last, None, Some("17")) {
                        result.ok = true;
                        self.saw_eot = true;
                    }
                }
            }
            Ok::<(), ClientError>(())
        }
        .await
        .err();

        if !result.ok {
            self.finalize_failure(&mut result, &last);
        } else {
            Self::set_success_message(&mut result);
        }
        self.finish().await; // Fin always disposes + closes, win or lose
        self.attach_trace(&mut result);
        match err {
            Some(e) => Err(e),
            None => Ok(result),
        }
    }

    /// Abort a POS-started purchase after a successful init that won't be completed.
    /// Safe to call even when there's nothing pending.
    pub async fn pos_starter_purchase_cancel(&mut self) -> Result<(), ClientError> {
        self.begin_txn();
        if self.transport.connected() {
            self.finish().await;
        }
        Ok(())
    }

    /// Query which operations the terminal is provisioned for (processing code 390000).
    /// The 390000 conversation, shared by `get_authorized_operations` and by
    /// `connection_test`'s fallback. Returns the raw DE48 flag string.
    async fn authorized_operations_inner(
        &mut self,
        result: &mut TransactionResult,
        last: &mut Option<Recv>,
    ) -> Result<String, ClientError> {
        self.ensure_connected().await?;
        let mut req = self.base_message("390000");
        let currency = self.cfg.currency.clone();
        req.set_str(49, &currency);
        *last = self.send_and_await_first_ack(&mut req).await?;
        let mut flags = String::new();
        if Self::validate(last, None, Some("15")) {
            let iso = last.as_ref().unwrap().iso.clone();
            self.harvest(result, &iso, &[(12, None), (13, None), (41, Some(Target::TerminalId))]);
            *last = self.recv(10_000).await?;
            if Self::validate(last, Some("390001"), None) {
                let iso = last.as_ref().unwrap().iso.clone();
                self.harvest(
                    result,
                    &iso,
                    &[
                        (39, Some(Target::ResponseCode)),
                        (46, Some(Target::TransactionDate)),
                        (48, None),
                        (57, Some(Target::PosVersion)),
                    ],
                );
                flags = iso.get_str(48).unwrap_or_default();
                result.ok = true;
            }
        }
        Ok(flags)
    }

    pub async fn get_authorized_operations(&mut self) -> Result<AuthorizedOperations, ClientError> {
        self.begin_txn();
        let mut result = TransactionResult::default();
        let mut last: Option<Recv> = None;
        let mut flags = String::new();

        let err = match self.authorized_operations_inner(&mut result, &mut last).await {
            Ok(f) => {
                flags = f;
                None
            }
            Err(e) => Some(e),
        };

        if !result.ok {
            self.finalize_failure(&mut result, &last);
        } else {
            Self::set_success_message(&mut result);
        }
        self.finish().await;
        self.attach_trace(&mut result);

        if let Some(e) = err {
            return Err(e);
        }
        // DE48 is a '*'-separated list of 0/1 flags, in this order.
        let parts: Vec<&str> = flags.split('*').collect();
        let at = |i: usize| parts.get(i).copied() == Some("1");
        Ok(AuthorizedOperations {
            balance: at(0),
            bill: at(1),
            report: at(2),
            mci_bill: at(3),
            pin_charge: at(4),
            purchase: at(5),
            topup_charge: at(6),
            tci_bill: at(7),
            payment_service: at(8),
            pos_version: result.pos_version.clone(),
            result,
        })
    }

    /// Aggregate totals report between two dates (processing code 380000).
    pub async fn total_report(
        &mut self,
        from_date: &str,
        to_date: &str,
        pos_pin: Option<&str>,
    ) -> Result<TotalReportResult, ClientError> {
        self.begin_txn();
        let mut result = TransactionResult::default();
        let mut last: Option<Recv> = None;
        let mut out = TotalReportResult::default();

        let err = async {
            self.ensure_connected().await?;
            let mut req = self.base_message("380000");
            let currency = self.cfg.currency.clone();
            req.set_str(48, "200");
            req.set_str(49, &currency);
            req.set_str(57, "4");
            req.set_str(58, "100");
            req.set_str(59, from_date);
            req.set_str(60, to_date);
            if let Some(pin) = pos_pin {
                if !pin.is_empty() {
                    req.set_str(61, pin);
                }
            }
            last = self.send_and_await_first_ack(&mut req).await?;
            if Self::validate(&last, None, Some("15")) {
                let iso = last.as_ref().unwrap().iso.clone();
                self.harvest(&mut result, &iso, &[(12, None), (13, None), (41, Some(Target::TerminalId))]);
                last = self.recv(120_000).await?;
                if Self::validate(&last, Some("380001"), None) {
                    let iso = last.as_ref().unwrap().iso.clone();
                    self.harvest(&mut result, &iso, &[(39, Some(Target::ResponseCode))]);
                    self.send_ack().await?;
                    let de48 = iso.get_str(48).unwrap_or_default();
                    let parts: Vec<&str> = de48.split('*').collect();
                    let num = |i: usize| -> u64 { js_parse_int(parts.get(i).copied().unwrap_or("")) };
                    out.bills_count = num(1);
                    out.bills_total_amount = num(2);
                    out.pin_charge_count = num(3);
                    out.pin_charge_amount = num(4);
                    out.topup_charge_count = num(5);
                    out.topup_charge_amount = num(6);
                    out.purchase_count = num(7);
                    out.purchase_amount = num(8);
                    out.group_charge_count = num(9);
                    out.group_charge_amount = num(10);
                    result.ok = true;
                }
            }
            Ok::<(), ClientError>(())
        }
        .await
        .err();

        if !result.ok {
            self.finalize_failure(&mut result, &last);
        } else {
            Self::set_success_message(&mut result);
        }
        self.finish().await;
        self.attach_trace(&mut result);
        match err {
            Some(e) => Err(e),
            None => {
                out.result = result;
                Ok(out)
            }
        }
    }

    /// Detailed transaction report (processing code 380000), streamed until EOF.
    pub async fn report(
        &mut self,
        filter: ReportFilter,
        filter_values: &[String],
        report_type: ReportType,
        pos_pin: Option<&str>,
    ) -> Result<ReportResult, ClientError> {
        self.begin_txn();
        let mut result = TransactionResult::default();
        let mut last: Option<Recv> = None;
        let mut payload = String::new();

        let fv = |i: usize| filter_values.get(i).cloned().unwrap_or_default();

        let err = async {
            self.ensure_connected().await?;
            let mut req = self.base_message("380000");
            let currency = self.cfg.currency.clone();
            req.set_str(48, "100");
            req.set_str(49, &currency);
            req.set_str(57, &report_type.to_string());
            match filter {
                1 => {
                    // Date
                    req.set_str(58, "100");
                    req.set_str(59, &fv(0));
                    req.set_str(60, &fv(1));
                }
                2 => {
                    // Serial
                    req.set_str(58, "200");
                    req.set_str(59, &fv(0));
                }
                3 => {
                    // Time
                    req.set_str(58, "300");
                    req.set_str(59, &fv(0));
                    req.set_str(60, &fv(1));
                    req.set_str(62, &fv(2));
                }
                _ => {}
            }
            if let Some(pin) = pos_pin {
                if !pin.is_empty() {
                    req.set_str(61, pin);
                }
            }
            last = self.send_and_await_first_ack(&mut req).await?;
            if Self::validate(&last, None, Some("15")) {
                let iso = last.as_ref().unwrap().iso.clone();
                self.harvest(&mut result, &iso, &[(12, None), (13, None), (41, Some(Target::TerminalId))]);
                loop {
                    last = self.recv(120_000).await?;
                    if !Self::validate(&last, Some("380001"), None) {
                        result.ok = false;
                        break;
                    }
                    let iso = last.as_ref().unwrap().iso.clone();
                    payload.push_str(&iso.get_str(48).unwrap_or_default());
                    result.ok = true;
                    let eof = iso.get_str(60);
                    self.send_ack().await?;
                    if eof.as_deref() == Some("100") {
                        break;
                    }
                }
            }
            Ok::<(), ClientError>(())
        }
        .await
        .err();

        if !result.ok {
            self.finalize_failure(&mut result, &last);
        } else {
            Self::set_success_message(&mut result);
        }
        self.finish().await;
        self.attach_trace(&mut result);
        match err {
            Some(e) => Err(e),
            None => Ok(ReportResult {
                rows: parse_report_rows(&payload, report_type),
                result,
            }),
        }
    }

    /// Disconnect the underlying channel.
    pub fn close(&mut self) {
        self.transport.close();
        self.last_closed_at = Some(Instant::now());
    }

    // ---------------------------------------------------------- shared flows

    async fn card_financial_flow<F>(
        &mut self,
        proc: &str,
        mid_proc: &str,
        card_fields: Option<HarvestMap<'_>>,
        build: F,
        final_fields: HarvestMap<'_>,
    ) -> Result<TransactionResult, ClientError>
    where
        F: FnOnce(&mut Iso8583Message) + Send,
    {
        self.begin_txn();
        let mut result = TransactionResult::default();
        let mut last: Option<Recv> = None;
        let card_fields: HarvestMap = card_fields.unwrap_or(&[
            (60, Some(Target::CardHash2)),
            (62, Some(Target::CardMask)),
            (63, Some(Target::CardHash1)),
        ]);

        let err = async {
            self.ensure_connected().await?;
            let mut req = self.base_message(proc);
            build(&mut req);
            last = self.send_and_await_first_ack(&mut req).await?;
            if Self::validate(&last, None, Some("15")) {
                let iso = last.as_ref().unwrap().iso.clone();
                self.harvest(&mut result, &iso, &[(12, None), (13, None), (41, Some(Target::TerminalId))]);
                last = self.recv(30_000).await?;
                if Self::validate(&last, Some(mid_proc), None) {
                    let iso = last.as_ref().unwrap().iso.clone();
                    self.harvest(&mut result, &iso, card_fields);
                    self.send_ack().await?;
                    last = self.recv(120_000).await?;
                    if Self::validate(&last, Some("000008"), None) {
                        let iso = last.as_ref().unwrap().iso.clone();
                        self.harvest(&mut result, &iso, final_fields);
                        self.send_ack().await?;
                        last = self.recv(10_000).await?;
                        if Self::validate(&last, None, Some("17")) {
                            result.ok = true;
                            self.saw_eot = true;
                        }
                    }
                }
            }
            Ok::<(), ClientError>(())
        }
        .await
        .err();

        if !result.ok {
            self.finalize_failure(&mut result, &last);
        } else {
            Self::set_success_message(&mut result);
        }
        self.finish().await;
        self.attach_trace(&mut result);
        match err {
            Some(e) => Err(e),
            None => Ok(result),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// JS `parseInt` semantics: parse the leading run of digits, 0 when there is none.
fn js_parse_int(s: &str) -> u64 {
    let digits: String = s.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u64>().unwrap_or(0)
}

/// Rows are groups of 10 '*'-separated tokens (11 for payment service reports).
fn parse_report_rows(payload: &str, report_type: ReportType) -> Vec<ReportRow> {
    if payload.is_empty() {
        return Vec::new();
    }
    let mut tokens: Vec<&str> = payload.split('*').collect();
    while tokens.last() == Some(&"") {
        tokens.pop();
    }
    let per_row = if report_type == 6 { 11 } else { 10 };
    let count = tokens.len() / per_row;
    let mut rows = Vec::with_capacity(count);
    let t = |i: usize| tokens.get(i).copied().unwrap_or("").to_string();
    for i in 0..count {
        let base = i * per_row;
        let mut row = ReportRow {
            date: t(base + 1),
            shift_code: t(base + 2),
            trace_number: t(base + 3),
            amount: 0,
            rrn: t(base + 5),
            bank: t(base + 6),
            card_mask: t(base + 7),
            card_hash: t(base + 8),
            bill_id: None,
            payment_id: None,
            terminal_id: t(base + per_row - 1),
        };
        row.amount = js_parse_int(tokens.get(base + 4).copied().unwrap_or(""));
        if report_type == 1 {
            // Bill rows pack cardHash=billId=paymentId into one token
            let f = t(base + 8);
            let mut segs = f.split('=');
            row.card_hash = segs.next().unwrap_or("").to_string();
            row.bill_id = Some(segs.next().unwrap_or("").to_string());
            row.payment_id = Some(segs.next().unwrap_or("").to_string());
        }
        rows.push(row);
    }
    rows
}

/// The fake terminal, shared with the provider-level tests in `super`.
#[cfg(test)]
pub mod tests_support {
    pub use super::tests::spawn_fake_terminal;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // ------------------------------------------------------- fake terminal

    /// Wrap an ISO message the way the terminal does on the way back:
    /// 2-byte big-endian length over header+body, then the 5-byte header.
    fn frame(iso: &[u8]) -> Vec<u8> {
        let mut out = ((5 + iso.len()) as u16).to_be_bytes().to_vec();
        out.extend_from_slice(&[0x60, 0x00, 0x00, 0x00, 0x00]);
        out.extend_from_slice(iso);
        out
    }

    fn msg(pairs: &[(u16, &str)]) -> Vec<u8> {
        let mut m = Iso8583Message::new();
        m.set_mti(MTI);
        for (de, v) in pairs {
            m.set_str(*de, v);
        }
        m.pack_with_mac().expect("pack")
    }

    /// A stand-in SSP1126 speaking the real framing over a real socket.
    /// Processing codes listed in `mute` are accepted and then ignored, reproducing
    /// a terminal that stays silent to one probe while the link is otherwise fine.
    pub async fn spawn_fake_terminal(mute: &'static [&'static str]) -> u16 {
        spawn_counting_terminal(mute).await.0
    }

    /// Same, but counts how many times a muted processing code reaches it — the
    /// terminal drops first messages, so the number of sends is what we measure.
    pub async fn spawn_counting_terminal(
        mute: &'static [&'static str],
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let sends = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let port = spawn_terminal_inner(mute, sends.clone()).await;
        (port, sends)
    }

    async fn spawn_terminal_inner(
        mute: &'static [&'static str],
        sends: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let sends = sends.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    loop {
                        let n = match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        let Ok(req) = Iso8583Message::unpack(&buf[..n]) else { continue };
                        let proc = req.get_str(3).unwrap_or_default();
                        if mute.contains(&proc.as_str()) {
                            sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            continue;
                        }
                        let replies: Vec<Vec<u8>> = match proc.as_str() {
                            "410000" => vec![
                                msg(&[(3, "410000"), (39, "15"), (41, "15527222")]),
                                msg(&[(3, "410003"), (39, "00")]),
                            ],
                            // The PC acknowledges the result; the terminal ends the
                            // conversation with 17.
                            "000004" => vec![msg(&[(3, "000004"), (39, "17")])],
                            "390000" => vec![
                                msg(&[(3, "390000"), (39, "15"), (41, "15527222")]),
                                msg(&[
                                    (3, "390001"),
                                    (39, "00"),
                                    (48, "1*1*1*1*1*1*1*1*1*"),
                                    (57, "10.063.00PO"),
                                ]),
                            ],
                            _ => vec![], // dispose and anything else: silence
                        };
                        for r in replies {
                            if sock.write_all(&frame(&r)).await.is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });
        port
    }

    /// Real timings would make these tests take tens of seconds.
    fn fast_config(port: u16) -> SamanConfig {
        SamanConfig {
            host: "127.0.0.1".into(),
            port,
            first_ack_timeout_ms: 250,
            retry_delay_ms: 10,
            min_reconnect_gap_ms: 0,
            reconnect_gap_after_partial_ms: 0,
            include_trace: true,
            ..SamanConfig::default()
        }
    }

    /// Measured on the real terminal: it drops the first message on a fresh
    /// connection often enough that one retry is not enough — both attempts were
    /// lost about one run in three. Waiting longer never helped (20s produced
    /// nothing at all), so the send count is the only lever that works.
    #[tokio::test]
    async fn a_dropped_first_message_is_retried_up_to_the_configured_count() {
        let (port, sends) = spawn_counting_terminal(&["390000"]).await;
        let mut cfg = fast_config(port);
        cfg.first_ack_attempts = 3;
        let mut c = SamanClient::new(cfg);

        let _ = c.get_authorized_operations().await;

        assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    /// One attempt means one message — nothing is re-sent behind the operator's back.
    #[tokio::test]
    async fn a_single_attempt_sends_the_message_once() {
        let (port, sends) = spawn_counting_terminal(&["390000"]).await;
        let mut cfg = fast_config(port);
        cfg.first_ack_attempts = 1;
        let mut c = SamanClient::new(cfg);

        let _ = c.get_authorized_operations().await;

        assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn connection_test_reports_which_probe_answered() {
        let port = spawn_fake_terminal(&[]).await;
        let mut c = SamanClient::new(fast_config(port));

        let r = c.connection_test().await.expect("transport must not error");

        assert!(r.ok, "410000 was answered, so the test should pass: {r:?}");
        assert_eq!(r.probe.as_deref(), Some("410000"));
    }

    /// A terminal that ignores 410000 while answering everything else must not be
    /// reported as dead by the one button an operator presses to check it.
    #[tokio::test]
    async fn connection_test_falls_back_when_410000_is_unanswered() {
        let port = spawn_fake_terminal(&["410000"]).await;
        let mut c = SamanClient::new(fast_config(port));

        let r = c.connection_test().await.expect("transport must not error");

        assert!(r.ok, "the fallback probe should have succeeded: {r:?}");
        assert_eq!(r.probe.as_deref(), Some("390000"));
        assert_eq!(r.terminal_id.as_deref(), Some("15527222"));
    }

    /// A terminal that answers nothing at all is still a failure.
    #[tokio::test]
    async fn connection_test_fails_when_no_probe_is_answered() {
        let port = spawn_fake_terminal(&["410000", "390000"]).await;
        let mut c = SamanClient::new(fast_config(port));

        let r = c.connection_test().await.expect("transport must not error");

        assert!(!r.ok, "nothing answered, so it must not report success: {r:?}");
        assert_eq!(r.timed_out, Some(true));
    }

    #[test]
    fn print_data_packing_matches_reference() {
        let items = vec![
            PrintData { alignment: 2, receipt: 0, title: "Order".into(), value: "1001".into() },
            PrintData { alignment: 1, receipt: 2, title: "A|B".into(), value: "x".into() },
        ];
        let packed = SamanClient::pack_print_data(Some(&items)).unwrap();
        // count=2, then per item: align, receipt, LLL title, title, LLL value, value
        assert_eq!(packed, "220005Order004100112003A B001x");
    }

    #[test]
    fn print_data_respects_budget_and_empty() {
        assert!(SamanClient::pack_print_data(None).is_none());
        assert!(SamanClient::pack_print_data(Some(&[])).is_none());
        let big = "x".repeat(400);
        let items = vec![PrintData { alignment: 0, receipt: 0, title: big.clone(), value: big }];
        assert!(SamanClient::pack_print_data(Some(&items)).is_none());
    }

    #[test]
    fn report_rows_parse_including_bill_variant() {
        // 10 tokens per row; token[0] is a leading marker, fields start at +1
        let payload = "m*0501*S1*T1*5000*RRN1*BankA*6037xx11*HASH1*TID1*";
        let rows = parse_report_rows(payload, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].date, "0501");
        assert_eq!(rows[0].amount, 5000);
        assert_eq!(rows[0].terminal_id, "TID1");

        let bill_payload = "m*0501*S1*T1*5000*RRN1*BankA*6037xx11*HASH=BILL=PAY*TID1*";
        let rows = parse_report_rows(bill_payload, 1);
        assert_eq!(rows[0].card_hash, "HASH");
        assert_eq!(rows[0].bill_id.as_deref(), Some("BILL"));
        assert_eq!(rows[0].payment_id.as_deref(), Some("PAY"));
    }

    #[test]
    fn config_defaults_mirror_library() {
        let c = SamanConfig::default();
        assert_eq!(c.port, 1197);
        assert_eq!(c.baud_rate, 19200);
        assert_eq!(c.currency, "364");
        assert_eq!(c.component_version, "1.4.3.0");
        assert!(c.verify_mac);
        assert_eq!(c.min_reconnect_gap_ms, 500);
        assert_eq!(c.reconnect_gap_after_partial_ms, 1500);
        assert_eq!(c.first_ack_timeout_ms, 3000);
        assert!(c.retry_on_first_timeout);
    }
}

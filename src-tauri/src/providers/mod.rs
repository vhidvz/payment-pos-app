//! Provider abstraction: every payment terminal integration implements
//! [`Provider`], exposing self-describing metadata, a function catalog with
//! typed parameter specs, a JSON-schema'd configuration surface, and a uniform
//! JSON invoke entry point. The REST API and the UI are generated from this —
//! nothing about a concrete provider is hardcoded anywhere else.

pub mod saman;
pub mod sandbox;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use utoipa::ToSchema;

// ------------------------------------------------------------------- errors

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("unknown function '{0}'")]
    UnknownFunction(String),
    #[error("invalid parameters: {0}")]
    InvalidParams(String),
    #[error("another transaction is in progress on this provider")]
    Busy,
    #[error("provider is not configured: {0}")]
    NotConfigured(String),
    #[error("{0}")]
    Execution(String),
}

// ----------------------------------------------------------------- metadata

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderMetadata {
    /// Stable provider identifier used in API routes.
    pub id: String,
    /// Human display name.
    pub name: String,
    pub vendor: String,
    /// Terminal model / family this provider drives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub description: String,
    /// Wire protocol summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Supported physical channels, e.g. ["tcp", "serial"].
    pub transports: Vec<String>,
    /// Capability tags (purchase, bill, charge, report, ...).
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParamSpec {
    pub name: String,
    /// JSON type: string | integer | number | boolean | array | object.
    #[serde(rename = "type")]
    pub ty: String,
    pub required: bool,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Closed set of allowed values, when applicable.
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Vec<Value>>,
    /// For arrays: JSON schema of one item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
}

impl ParamSpec {
    pub fn new(name: &str, ty: &str, required: bool, description: &str) -> Self {
        Self {
            name: name.into(),
            ty: ty.into(),
            required,
            description: description.into(),
            default: None,
            allowed: None,
            items: None,
            example: None,
        }
    }
    pub fn with_default(mut self, v: Value) -> Self {
        self.default = Some(v);
        self
    }
    pub fn with_enum(mut self, vs: Vec<Value>) -> Self {
        self.allowed = Some(vs);
        self
    }
    pub fn with_items(mut self, schema: Value) -> Self {
        self.items = Some(schema);
        self
    }
    pub fn with_example(mut self, v: Value) -> Self {
        self.example = Some(v);
        self
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FunctionSpec {
    /// Function identifier used in API routes (camelCase, mirrors the reference SDK).
    pub id: String,
    pub title: String,
    /// transaction | inquiry | report | utility | session | diagnostics
    pub category: String,
    pub summary: String,
    pub description: String,
    pub params: Vec<ParamSpec>,
    /// Prose description of the response shape.
    pub returns: String,
    /// False for local helpers that never touch the terminal.
    pub requires_terminal: bool,
    /// True when the call can block for a long time (e.g. waiting for a card tap).
    pub long_running: bool,
    /// Example request body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub configured: bool,
    /// Human-readable hint about what is missing when not configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration_hint: Option<String>,
    /// True while a session/connection is being held open (e.g. between
    /// posStarterPurchaseInit and posStarterPurchaseFin).
    pub session_open: bool,
    /// True while a transaction is currently executing.
    pub busy: bool,
}

// -------------------------------------------------------------------- trait

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn metadata(&self) -> ProviderMetadata;
    fn functions(&self) -> Vec<FunctionSpec>;
    fn function(&self, id: &str) -> Option<FunctionSpec> {
        self.functions().into_iter().find(|f| f.id == id)
    }
    /// JSON Schema describing this provider's configuration object.
    fn config_schema(&self) -> Value;
    async fn get_config(&self) -> Value;
    async fn set_config(&self, config: Value) -> Result<(), ProviderError>;
    async fn status(&self) -> ProviderStatus;
    async fn invoke(&self, function: &str, params: Value) -> Result<Value, ProviderError>;
}

// ----------------------------------------------------------------- registry

pub struct ProviderRegistry {
    providers: Vec<Arc<dyn Provider>>,
    by_id: HashMap<&'static str, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        let by_id = providers.iter().map(|p| (p.id(), p.clone())).collect();
        Self { providers, by_id }
    }

    pub fn all(&self) -> &[Arc<dyn Provider>] {
        &self.providers
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.by_id.get(id).cloned()
    }
}

// --------------------------------------------- shared terminal function set
//
// Both the Saman SSP1126 provider and the sandbox simulator expose this exact
// catalog (the sandbox mirrors the real provider so integrations can be
// developed and demoed without hardware).

fn print_data_item_schema() -> Value {
    json!({
        "type": "object",
        "required": ["alignment", "receipt", "title", "value"],
        "properties": {
            "alignment": { "type": "integer", "enum": [0, 1, 2], "description": "0=Right, 1=Left, 2=Center" },
            "receipt":   { "type": "integer", "enum": [0, 1, 2], "description": "0=Customer, 1=Merchant, 2=Both" },
            "title":     { "type": "string" },
            "value":     { "type": "string" }
        }
    })
}

fn additional_data_param() -> ParamSpec {
    ParamSpec::new(
        "additionalData",
        "array",
        false,
        "Extra receipt lines packed into DE48 (max 9 items / ~350 chars; '|' is replaced with a space).",
    )
    .with_items(print_data_item_schema())
    .with_example(json!([{ "alignment": 2, "receipt": 2, "title": "Order", "value": "1001" }]))
}

fn reference_data_param() -> ParamSpec {
    ParamSpec::new(
        "referenceData",
        "string",
        false,
        "Your own reference/order number, carried in DE56 and echoed on the receipt. Use this — not purchaseId — for order numbers.",
    )
    .with_example(json!("ORDER-1001"))
}

fn transaction_result_returns() -> String {
    "TransactionResult: { ok, responseCode, responseMessage, terminalId, traceNumber, serialId, rrn, \
     amount, effectiveAmount, transactionDate, cardMask, cardHash1, cardHash2, chargePin, chargeSerial, \
     chargeEmergencyNumber, posVersion, fields (raw DEs), timedOut }"
        .to_string()
}

pub fn terminal_function_specs() -> Vec<FunctionSpec> {
    let tr = transaction_result_returns();
    vec![
        FunctionSpec {
            id: "connectionTest".into(),
            title: "Connection test".into(),
            category: "diagnostics".into(),
            summary: "Verify reachability and protocol handshake with the terminal.".into(),
            description: "Runs the 410000 handshake conversation end-to-end (request → accept → result → ack → finish). \
                          A successful test proves the transport, framing and MAC are all working.".into(),
            params: vec![],
            returns: tr.clone(),
            requires_terminal: true,
            long_running: false,
            example: Some(json!({})),
        },
        FunctionSpec {
            id: "getAuthorizedOperations".into(),
            title: "Authorized operations".into(),
            category: "diagnostics".into(),
            summary: "Which operations the terminal is provisioned for.".into(),
            description: "Processing code 390000. Returns a flag per operation (balance, bill, report, mciBill, pinCharge, \
                          purchase, topupCharge, tciBill, paymentService) plus the terminal firmware version. Note: identified \
                          purchases (DE63) have no flag of their own — a terminal can report purchase=true and still refuse \
                          DE63-bearing purchases with code 07.".into(),
            params: vec![],
            returns: "{ balance, bill, report, mciBill, pinCharge, purchase, topupCharge, tciBill, paymentService, posVersion, result }".into(),
            requires_terminal: true,
            long_running: false,
            example: Some(json!({})),
        },
        FunctionSpec {
            id: "balance".into(),
            title: "Card balance".into(),
            category: "inquiry".into(),
            summary: "Card balance inquiry (customer taps/inserts card and enters PIN).".into(),
            description: "Processing code 310000. The terminal prompts for a card; the flow blocks until the cardholder acts \
                          or the terminal times out.".into(),
            params: vec![],
            returns: tr.clone(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({})),
        },
        FunctionSpec {
            id: "purchase".into(),
            title: "Purchase (PC-started)".into(),
            category: "transaction".into(),
            summary: "Sale where the PC sends the amount first, then the customer presents their card.".into(),
            description: "Processing code 000000, DE46=300. IMPORTANT: do not set purchaseId for an ordinary sale — it requests \
                          an *identified purchase* (a separately-provisioned transaction type) and unentitled merchants get \
                          responseCode 07 immediately. Use referenceData for your own order number.".into(),
            params: vec![
                ParamSpec::new("mainAmount", "integer", true, "Sale amount in Rials (DE4).").with_example(json!(10000)),
                ParamSpec::new("amounts", "string", false, "Optional split-amount string (DE47), terminal-defined format."),
                ParamSpec::new("purchaseId", "string", false, "ONLY for an identified purchase (DE63). Leave unset for a normal sale — see the function description."),
                ParamSpec::new("terminalId", "string", false, "Override the target terminal id (DE41)."),
                additional_data_param(),
                reference_data_param(),
            ],
            returns: tr.clone(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({ "mainAmount": 10000, "referenceData": "ORDER-1001" })),
        },
        FunctionSpec {
            id: "posStarterPurchaseInit".into(),
            title: "POS-started purchase — init".into(),
            category: "transaction".into(),
            summary: "Step 1: the customer taps their card first; the terminal reports routing options.".into(),
            description: "Processing code 000000, DE46=100. On success the connection is deliberately LEFT OPEN so \
                          posStarterPurchaseFin can complete the same transaction. Call posStarterPurchaseCancel if you \
                          decide not to proceed; don't invoke other functions in between.".into(),
            params: vec![],
            returns: "TransactionResult + segments: [{ code, label }] — routing options read off the card (usually one).".into(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({})),
        },
        FunctionSpec {
            id: "posStarterPurchaseFin".into(),
            title: "POS-started purchase — finalize".into(),
            category: "transaction".into(),
            summary: "Step 2: send the amount and complete the purchase started with init.".into(),
            description: "Processing code 000000, DE46=200. Runs on the connection init left open, and always disposes + closes \
                          when done, win or lose. Pass `segment` only when init returned more than one option.".into(),
            params: vec![
                ParamSpec::new("mainAmount", "integer", true, "Sale amount in Rials (DE4).").with_example(json!(10000)),
                ParamSpec::new("amounts", "string", false, "Optional split-amount string (DE47)."),
                ParamSpec::new("segment", "string", false, "The `code` of a segment returned by init; omit when there was zero or one option."),
                ParamSpec::new("purchaseId", "string", false, "ONLY for an identified purchase (DE63)."),
                ParamSpec::new("terminalId", "string", false, "Override the target terminal id (DE41)."),
                additional_data_param(),
                reference_data_param(),
            ],
            returns: tr.clone(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({ "mainAmount": 10000 })),
        },
        FunctionSpec {
            id: "posStarterPurchaseCancel".into(),
            title: "POS-started purchase — cancel".into(),
            category: "session".into(),
            summary: "Abort an init that won't be completed; disposes and closes the held connection.".into(),
            description: "Safe to call even when there's nothing pending.".into(),
            params: vec![],
            returns: "{ ok: true }".into(),
            requires_terminal: true,
            long_running: false,
            example: Some(json!({})),
        },
        FunctionSpec {
            id: "billPayment".into(),
            title: "Bill payment".into(),
            category: "transaction".into(),
            summary: "Pay a utility bill by billId + paymentId.".into(),
            description: "Processing code 400000, DE46=200. The amount is encoded inside paymentId — validate the pair locally \
                          with validateBill first and show the decoded amount to the user before paying.".into(),
            params: vec![
                ParamSpec::new("billId", "string", true, "Bill identifier (شناسه قبض), 6-13 digits.").with_example(json!("1234567890123")),
                ParamSpec::new("paymentId", "string", true, "Payment identifier (شناسه پرداخت), 6-13 digits; encodes the amount and two check digits.").with_example(json!("1234567890")),
                additional_data_param(),
                reference_data_param(),
            ],
            returns: tr.clone(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({ "billId": "1234567890123", "paymentId": "1234567890" })),
        },
        FunctionSpec {
            id: "billRequest".into(),
            title: "Bill request (from card)".into(),
            category: "inquiry".into(),
            summary: "Read bill data stored on the inserted card.".into(),
            description: "Processing code 400000, DE46=300.".into(),
            params: vec![],
            returns: tr.clone(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({})),
        },
        FunctionSpec {
            id: "pinCharge".into(),
            title: "PIN charge voucher".into(),
            category: "transaction".into(),
            summary: "Buy a prepaid PIN charge voucher.".into(),
            description: "Processing code 300000. The voucher PIN/serial/emergency number come back as chargePin / chargeSerial / \
                          chargeEmergencyNumber.".into(),
            params: vec![additional_data_param(), reference_data_param()],
            returns: tr.clone(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({})),
        },
        FunctionSpec {
            id: "topupCharge".into(),
            title: "Direct top-up".into(),
            category: "transaction".into(),
            summary: "Direct mobile top-up for a phone number.".into(),
            description: "Processing code 370000; the phone number goes in DE60.".into(),
            params: vec![
                ParamSpec::new("mobileNumber", "string", true, "Mobile number to top up.").with_example(json!("09121234567")),
                additional_data_param(),
                reference_data_param(),
            ],
            returns: tr.clone(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({ "mobileNumber": "09121234567" })),
        },
        FunctionSpec {
            id: "mciBillInquiry".into(),
            title: "MCI bill inquiry".into(),
            category: "inquiry".into(),
            summary: "MCI (Hamrah-e-Aval) mobile bill inquiry.".into(),
            description: "Processing code 170000, DE46=100; the phone number goes in DE60 and the bill type in DE61.".into(),
            params: vec![
                ParamSpec::new("mciNumber", "string", true, "MCI mobile number.").with_example(json!("09121234567")),
                ParamSpec::new("billType", "integer", true, "1=MidTerm, 2=FullTerm.")
                    .with_enum(vec![json!(1), json!(2)])
                    .with_example(json!(1)),
            ],
            returns: tr.clone(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({ "mciNumber": "09121234567", "billType": 1 })),
        },
        FunctionSpec {
            id: "tciBillInquiry".into(),
            title: "TCI bill inquiry".into(),
            category: "inquiry".into(),
            summary: "TCI (telecom) landline bill inquiry.".into(),
            description: "Processing code 180000, DE46=100; number in DE60, type in DE61.".into(),
            params: vec![
                ParamSpec::new("tciNumber", "string", true, "TCI phone number.").with_example(json!("02188888888")),
                ParamSpec::new("billType", "integer", true, "1=MidTerm, 2=FullTerm.")
                    .with_enum(vec![json!(1), json!(2)])
                    .with_example(json!(1)),
            ],
            returns: tr.clone(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({ "tciNumber": "02188888888", "billType": 1 })),
        },
        FunctionSpec {
            id: "totalReport".into(),
            title: "Totals report".into(),
            category: "report".into(),
            summary: "Aggregate counts and amounts between two dates.".into(),
            description: "Processing code 380000 (DE48=200). Dates are terminal-format date strings (typically YYMMDD).".into(),
            params: vec![
                ParamSpec::new("fromDate", "string", true, "Start date, terminal format (typically YYMMDD).").with_example(json!("050101")),
                ParamSpec::new("toDate", "string", true, "End date, terminal format (typically YYMMDD).").with_example(json!("050131")),
                ParamSpec::new("posPin", "string", false, "POS report PIN, when the terminal demands one."),
            ],
            returns: "{ billsCount, billsTotalAmount, pinChargeCount, pinChargeAmount, topupChargeCount, topupChargeAmount, \
                      purchaseCount, purchaseAmount, groupChargeCount, groupChargeAmount, result }".into(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({ "fromDate": "050101", "toDate": "050131" })),
        },
        FunctionSpec {
            id: "report".into(),
            title: "Detailed report".into(),
            category: "report".into(),
            summary: "Detailed transaction rows, streamed from the terminal until EOF.".into(),
            description: "Processing code 380000 (DE48=100). Filter by date range, serial, or time window.".into(),
            params: vec![
                ParamSpec::new("filter", "integer", true, "1=Date (from,to), 2=Serial (serial), 3=Time (from,to,time).")
                    .with_enum(vec![json!(1), json!(2), json!(3)])
                    .with_example(json!(1)),
                ParamSpec::new("filterValues", "array", true, "Filter values matching the chosen filter (see `filter`).")
                    .with_items(json!({ "type": "string" }))
                    .with_example(json!(["050101", "050131"])),
                ParamSpec::new("reportType", "integer", true, "0=Purchase, 1=Bill, 2=PinCharge, 3=TopupCharge, 6=PaymentService.")
                    .with_enum(vec![json!(0), json!(1), json!(2), json!(3), json!(6)])
                    .with_example(json!(0)),
                ParamSpec::new("posPin", "string", false, "POS report PIN, when the terminal demands one."),
            ],
            returns: "{ rows: [{ date, shiftCode, traceNumber, amount, rrn, bank, cardMask, cardHash, billId?, paymentId?, terminalId }], result }".into(),
            requires_terminal: true,
            long_running: true,
            example: Some(json!({ "filter": 1, "filterValues": ["050101", "050131"], "reportType": 0 })),
        },
        FunctionSpec {
            id: "validateBill".into(),
            title: "Validate bill pair".into(),
            category: "utility".into(),
            summary: "Locally validate a billId/paymentId pair and decode amount + category.".into(),
            description: "Pure local computation (mod-11 check digits; the amount is encoded in the payment id). \
                          No terminal round-trip.".into(),
            params: vec![
                ParamSpec::new("billId", "string", true, "Bill identifier."),
                ParamSpec::new("paymentId", "string", true, "Payment identifier."),
            ],
            returns: "{ ok, reason?, billId, paymentId, amountRials, category, categoryEn, categoryFa }".into(),
            requires_terminal: false,
            long_running: false,
            example: Some(json!({ "billId": "1234567890123", "paymentId": "1234567890" })),
        },
        FunctionSpec {
            id: "billAmountRials".into(),
            title: "Decode bill amount".into(),
            category: "utility".into(),
            summary: "Decode the amount (Rials) encoded inside a payment id.".into(),
            description: "Pure local computation.".into(),
            params: vec![ParamSpec::new("paymentId", "string", true, "Payment identifier.")],
            returns: "{ amountRials }".into(),
            requires_terminal: false,
            long_running: false,
            example: Some(json!({ "paymentId": "1234567890" })),
        },
        FunctionSpec {
            id: "billCategory".into(),
            title: "Bill category".into(),
            category: "utility".into(),
            summary: "Category of a bill id (water, electricity, gas, ...).".into(),
            description: "Pure local computation from the second-to-last digit of the bill id.".into(),
            params: vec![ParamSpec::new("billId", "string", true, "Bill identifier.")],
            returns: "{ code, en, fa }".into(),
            requires_terminal: false,
            long_running: false,
            example: Some(json!({ "billId": "1234567890123" })),
        },
    ]
}

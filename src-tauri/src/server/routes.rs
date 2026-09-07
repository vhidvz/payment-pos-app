//! REST API routes + OpenAPI document.
//!
//! Everything is generated from the provider abstraction: provider metadata,
//! function catalogs, parameter specs and invocation all flow through the
//! generic `Provider` trait — no provider-specific route exists.

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use crate::activity::ActivityEntry;
use crate::providers::{
    FunctionSpec, LinkState, LinkStatus, Provider, ProviderError, ProviderMetadata, ProviderStatus,
};
use crate::security::UnlockError;
use crate::settings::Settings;

use super::{AppState, ServerStatus};

// ------------------------------------------------------------------- errors

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    /// Machine-readable error code.
    pub code: String,
    /// Human-readable message.
    pub error: String,
}

pub struct ApiFailure(StatusCode, ApiError);

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

fn fail(status: StatusCode, code: &str, msg: impl Into<String>) -> ApiFailure {
    ApiFailure(
        status,
        ApiError {
            code: code.into(),
            error: msg.into(),
        },
    )
}

fn provider_error(e: ProviderError) -> ApiFailure {
    match &e {
        ProviderError::UnknownFunction(_) => fail(StatusCode::NOT_FOUND, "unknown_function", e.to_string()),
        ProviderError::InvalidParams(_) => fail(StatusCode::BAD_REQUEST, "invalid_params", e.to_string()),
        ProviderError::Busy => fail(StatusCode::CONFLICT, "busy", e.to_string()),
        ProviderError::NotConfigured(_) => {
            fail(StatusCode::PRECONDITION_FAILED, "not_configured", e.to_string())
        }
        ProviderError::Execution(_) => fail(StatusCode::BAD_GATEWAY, "execution_failed", e.to_string()),
        ProviderError::Unreachable(_) => fail(StatusCode::SERVICE_UNAVAILABLE, "terminal_unreachable", e.to_string()),
    }
}

// ---------------------------------------------------------------- auth gate

/// Reads the bearer token, if the caller presented one.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    Some(raw.strip_prefix("Bearer ")?.trim().to_string())
}

/// Present on a handler only when the caller may change things.
///
/// An extractor rather than a middleware layer, because protected and open routes
/// share paths — `GET /api/v1/settings` is open, `PUT` is not — so a path-scoped
/// layer would not fit. Declare it before any body-consuming extractor.
pub struct Unlocked;

impl FromRequestParts<AppState> for Unlocked {
    type Rejection = ApiFailure;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if state.auth.is_unlocked(bearer_token(&parts.headers).as_deref()) {
            Ok(Unlocked)
        } else {
            Err(fail(
                StatusCode::UNAUTHORIZED,
                "locked",
                "this application is locked",
            ))
        }
    }
}

fn provider_or_404(
    state: &AppState,
    id: &str,
) -> Result<std::sync::Arc<dyn Provider>, ApiFailure> {
    state
        .registry
        .get(id)
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "unknown_provider", format!("unknown provider '{id}'")))
}

fn active_provider(state: &AppState) -> Result<std::sync::Arc<dyn Provider>, ApiFailure> {
    let active = state.settings.get().providers.active;
    provider_or_404(state, &active)
}

// ---------------------------------------------------------------- responses

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_secs: u64,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub arch: String,
    pub active_provider: String,
    pub server: ServerStatus,
    pub docs_path: String,
    pub openapi_path: String,
    pub settings_file: String,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    #[serde(flatten)]
    pub metadata: ProviderMetadata,
    pub active: bool,
    pub status: ProviderStatus,
    pub function_count: usize,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDetail {
    pub metadata: ProviderMetadata,
    pub active: bool,
    pub status: ProviderStatus,
    /// JSON Schema for this provider's configuration object.
    #[schema(value_type = Object)]
    pub config_schema: Value,
    /// Current configuration.
    #[schema(value_type = Object)]
    pub config: Value,
    pub functions: Vec<FunctionSpec>,
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetActiveRequest {
    /// Provider id to make active.
    pub id: String,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdateResponse {
    pub settings: Settings,
    /// True when the HTTP server is rebinding because server settings changed —
    /// the API may briefly drop and come back on the new host/port.
    pub server_restarting: bool,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    /// Whether an administrator has set an application password.
    pub password_set: bool,
    /// Whether *this caller* is currently locked out of changes.
    pub locked: bool,
    /// Idle minutes before an unlocked session re-locks. 0 disables expiry.
    pub auto_lock_minutes: u32,
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnlockRequest {
    pub password: String,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnlockResponse {
    /// Present this as `Authorization: Bearer <token>` on protected routes.
    pub token: String,
    pub auto_lock_minutes: u32,
}

#[derive(Deserialize)]
pub struct ActivityQuery {
    pub limit: Option<usize>,
}

// ----------------------------------------------------------------- handlers

#[utoipa::path(get, path = "/api/v1/health", tag = "system",
    responses((status = 200, description = "Service is up", body = HealthResponse)))]
async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        uptime_secs: state.started_at.elapsed().as_secs(),
    })
}

#[utoipa::path(get, path = "/api/v1/system", tag = "system",
    responses((status = 200, description = "Runtime information", body = SystemInfo)))]
async fn system(State(state): State<AppState>) -> Json<SystemInfo> {
    let settings = state.settings.get();
    Json(SystemInfo {
        name: "Ledger POS".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        platform: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        active_provider: settings.providers.active,
        server: state.status(),
        docs_path: "/docs".into(),
        openapi_path: "/api-docs/openapi.json".into(),
        settings_file: crate::settings::settings_path().display().to_string(),
    })
}

#[utoipa::path(get, path = "/api/v1/settings", tag = "settings",
    responses((status = 200, description = "Current settings", body = Settings)))]
async fn get_settings(State(state): State<AppState>) -> Json<Settings> {
    Json(state.settings.get())
}

#[utoipa::path(put, path = "/api/v1/settings", tag = "settings",
    request_body = Settings,
    responses(
        (status = 200, description = "Settings updated", body = SettingsUpdateResponse),
        (status = 400, description = "Validation failed", body = ApiError)))]
async fn put_settings(
    State(state): State<AppState>,
    _: Unlocked,
    Json(body): Json<Settings>,
) -> Result<Json<SettingsUpdateResponse>, ApiFailure> {
    if state.registry.get(&body.providers.active).is_none() {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "unknown_provider",
            format!("providers.active '{}' is not an installed provider", body.providers.active),
        ));
    }
    // Apply any changed provider configs to the live providers BEFORE persisting,
    // so the file and the runtime can't diverge (and invalid configs are rejected).
    for (id, cfg) in &body.providers.configs {
        let Some(p) = state.registry.get(id) else {
            return Err(fail(
                StatusCode::BAD_REQUEST,
                "unknown_provider",
                format!("providers.configs contains unknown provider '{id}'"),
            ));
        };
        if p.get_config().await != *cfg {
            p.set_config(cfg.clone()).await.map_err(|e| {
                let f = provider_error(e);
                ApiFailure(f.0, ApiError {
                    code: f.1.code,
                    error: format!("provider '{id}': {}", f.1.error),
                })
            })?;
        }
    }
    let before = state.settings.get();
    let next = state
        .settings
        .update(|s| *s = body)
        .map_err(|e| fail(StatusCode::BAD_REQUEST, "invalid_settings", e.to_string()))?;
    let server_restarting = before.server != next.server;
    Ok(Json(SettingsUpdateResponse {
        settings: next,
        server_restarting,
    }))
}

#[utoipa::path(get, path = "/api/v1/auth/status", tag = "security",
    responses((status = 200, description = "Whether a password is set and whether this caller is locked", body = AuthStatus)))]
async fn auth_status(State(state): State<AppState>, headers: HeaderMap) -> Json<AuthStatus> {
    let admin = state.auth.admin();
    let unlocked = state.auth.is_unlocked(bearer_token(&headers).as_deref());
    Json(AuthStatus {
        password_set: admin.password_hash.is_some(),
        locked: !unlocked,
        auto_lock_minutes: admin.auto_lock_minutes,
    })
}

#[utoipa::path(post, path = "/api/v1/auth/unlock", tag = "security",
    request_body = UnlockRequest,
    responses(
        (status = 200, description = "Unlocked; use the token on protected routes", body = UnlockResponse),
        (status = 401, description = "Incorrect password", body = ApiError),
        (status = 412, description = "No password is set, so there is nothing to unlock", body = ApiError)))]
async fn auth_unlock(
    State(state): State<AppState>,
    Json(body): Json<UnlockRequest>,
) -> Result<Json<UnlockResponse>, ApiFailure> {
    match state.auth.unlock(&body.password).await {
        Ok(token) => Ok(Json(UnlockResponse {
            token,
            auto_lock_minutes: state.auth.admin().auto_lock_minutes,
        })),
        Err(UnlockError::NoPasswordSet) => Err(fail(
            StatusCode::PRECONDITION_FAILED,
            "no_password_set",
            "no application password is set; nothing to unlock",
        )),
        Err(UnlockError::InvalidPassword) => Err(fail(
            StatusCode::UNAUTHORIZED,
            "invalid_password",
            "incorrect password",
        )),
    }
}

#[utoipa::path(post, path = "/api/v1/auth/lock", tag = "security",
    responses((status = 200, description = "The presented token is revoked", body = Object)))]
async fn auth_lock(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    if let Some(token) = bearer_token(&headers) {
        state.auth.lock(&token);
    }
    Json(json!({ "ok": true }))
}

#[utoipa::path(get, path = "/api/v1/activity", tag = "system",
    params(("limit" = Option<usize>, Query, description = "Max entries (default 50, max 200)")),
    responses((status = 200, description = "Recent invocations, newest first", body = [ActivityEntry])))]
async fn activity(
    State(state): State<AppState>,
    Query(q): Query<ActivityQuery>,
) -> Json<Vec<ActivityEntry>> {
    let limit = q.limit.unwrap_or(50).min(200);
    Json(state.activity.list(limit))
}

async fn provider_summary(state: &AppState, p: &std::sync::Arc<dyn Provider>) -> ProviderSummary {
    let active_id = state.settings.get().providers.active;
    ProviderSummary {
        metadata: p.metadata(),
        active: p.id() == active_id,
        status: p.status().await,
        function_count: p.functions().len(),
    }
}

#[utoipa::path(get, path = "/api/v1/providers", tag = "providers",
    responses((status = 200, description = "Installed providers", body = [ProviderSummary])))]
async fn list_providers(State(state): State<AppState>) -> Json<Vec<ProviderSummary>> {
    let mut out = Vec::new();
    for p in state.registry.all() {
        out.push(provider_summary(&state, p).await);
    }
    Json(out)
}

async fn provider_detail_of(
    state: &AppState,
    p: std::sync::Arc<dyn Provider>,
) -> ProviderDetail {
    let active_id = state.settings.get().providers.active;
    ProviderDetail {
        metadata: p.metadata(),
        active: p.id() == active_id,
        status: p.status().await,
        config_schema: p.config_schema(),
        config: p.get_config().await,
        functions: p.functions(),
    }
}

#[utoipa::path(get, path = "/api/v1/providers/active", tag = "providers",
    responses(
        (status = 200, description = "The active provider, in full detail", body = ProviderDetail),
        (status = 404, description = "Active provider not installed", body = ApiError)))]
async fn get_active_provider(
    State(state): State<AppState>,
) -> Result<Json<ProviderDetail>, ApiFailure> {
    let p = active_provider(&state)?;
    Ok(Json(provider_detail_of(&state, p).await))
}

#[utoipa::path(put, path = "/api/v1/providers/active", tag = "providers",
    request_body = SetActiveRequest,
    responses(
        (status = 200, description = "Active provider changed", body = ProviderDetail),
        (status = 404, description = "Unknown provider", body = ApiError)))]
async fn set_active_provider(
    State(state): State<AppState>,
    _: Unlocked,
    Json(body): Json<SetActiveRequest>,
) -> Result<Json<ProviderDetail>, ApiFailure> {
    let p = provider_or_404(&state, &body.id)?;
    state
        .settings
        .update(|s| s.providers.active = body.id.clone())
        .map_err(|e| fail(StatusCode::BAD_REQUEST, "invalid_settings", e.to_string()))?;
    Ok(Json(provider_detail_of(&state, p).await))
}

#[utoipa::path(get, path = "/api/v1/providers/{id}", tag = "providers",
    params(("id" = String, Path, description = "Provider id")),
    responses(
        (status = 200, description = "Provider metadata, status, config and functions", body = ProviderDetail),
        (status = 404, description = "Unknown provider", body = ApiError)))]
async fn get_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderDetail>, ApiFailure> {
    let p = provider_or_404(&state, &id)?;
    Ok(Json(provider_detail_of(&state, p).await))
}

#[utoipa::path(get, path = "/api/v1/providers/{id}/functions", tag = "functions",
    params(("id" = String, Path, description = "Provider id")),
    responses(
        (status = 200, description = "Function catalog", body = [FunctionSpec]),
        (status = 404, description = "Unknown provider", body = ApiError)))]
async fn list_functions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<FunctionSpec>>, ApiFailure> {
    let p = provider_or_404(&state, &id)?;
    Ok(Json(p.functions()))
}

#[utoipa::path(get, path = "/api/v1/providers/{id}/functions/{function}", tag = "functions",
    params(
        ("id" = String, Path, description = "Provider id"),
        ("function" = String, Path, description = "Function id")),
    responses(
        (status = 200, description = "Full function spec: params, types, defaults, examples", body = FunctionSpec),
        (status = 404, description = "Unknown provider or function", body = ApiError)))]
async fn get_function(
    State(state): State<AppState>,
    Path((id, function)): Path<(String, String)>,
) -> Result<Json<FunctionSpec>, ApiFailure> {
    let p = provider_or_404(&state, &id)?;
    p.function(&function)
        .map(Json)
        .ok_or_else(|| fail(StatusCode::NOT_FOUND, "unknown_function", format!("unknown function '{function}'")))
}

async fn invoke_on(
    state: &AppState,
    p: std::sync::Arc<dyn Provider>,
    function: String,
    params: Value,
) -> Result<Json<Value>, ApiFailure> {
    // Reject unknown functions before invoking so the activity log stays clean.
    if p.function(&function).is_none() {
        return Err(fail(
            StatusCode::NOT_FOUND,
            "unknown_function",
            format!("unknown function '{function}'"),
        ));
    }
    let started = std::time::Instant::now();
    let outcome = p.invoke(&function, params).await;
    let duration_ms = started.elapsed().as_millis() as u64;

    let entry = match &outcome {
        Ok(v) => ActivityEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Local::now().to_rfc3339(),
            provider: p.id().to_string(),
            function: function.clone(),
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(true),
            response_code: v
                .get("responseCode")
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| {
                    v.get("result")
                        .and_then(|r| r.get("responseCode"))
                        .and_then(Value::as_str)
                        .map(String::from)
                }),
            summary: v
                .get("responseMessage")
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| {
                    v.get("result")
                        .and_then(|r| r.get("responseMessage"))
                        .and_then(Value::as_str)
                        .map(String::from)
                }),
            duration_ms,
            error: None,
        },
        Err(e) => ActivityEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Local::now().to_rfc3339(),
            provider: p.id().to_string(),
            function: function.clone(),
            ok: false,
            response_code: None,
            summary: None,
            duration_ms,
            error: Some(e.to_string()),
        },
    };
    state.activity.push(entry);

    outcome.map(Json).map_err(provider_error)
}

#[utoipa::path(post, path = "/api/v1/providers/{id}/functions/{function}/invoke", tag = "functions",
    params(
        ("id" = String, Path, description = "Provider id"),
        ("function" = String, Path, description = "Function id")),
    request_body(content = Object, description = "Function parameters as a JSON object (see the function spec)"),
    responses(
        (status = 200, description = "Function result (shape per function spec `returns`)", body = Object),
        (status = 400, description = "Invalid parameters", body = ApiError),
        (status = 404, description = "Unknown provider or function", body = ApiError),
        (status = 409, description = "Another transaction is in progress", body = ApiError),
        (status = 412, description = "Provider not configured", body = ApiError),
        (status = 502, description = "Terminal/provider execution failure", body = ApiError)))]
async fn invoke_function(
    State(state): State<AppState>,
    Path((id, function)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiFailure> {
    let p = provider_or_404(&state, &id)?;
    let params = parse_params(&body)?;
    invoke_on(&state, p, function, params).await
}

/// An absent/empty body means "no parameters"; anything present must be valid JSON.
fn parse_params(body: &[u8]) -> Result<Value, ApiFailure> {
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(body).map_err(|e| {
        fail(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            format!("request body is not valid JSON: {e}"),
        )
    })
}

#[utoipa::path(post, path = "/api/v1/providers/active/functions/{function}/invoke", tag = "functions",
    params(("function" = String, Path, description = "Function id")),
    request_body(content = Object, description = "Function parameters as a JSON object"),
    responses(
        (status = 200, description = "Function result", body = Object),
        (status = 400, description = "Invalid parameters", body = ApiError),
        (status = 404, description = "Unknown function or active provider missing", body = ApiError),
        (status = 409, description = "Another transaction is in progress", body = ApiError),
        (status = 412, description = "Provider not configured", body = ApiError),
        (status = 502, description = "Terminal/provider execution failure", body = ApiError)))]
async fn invoke_on_active(
    State(state): State<AppState>,
    Path(function): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiFailure> {
    let p = active_provider(&state)?;
    let params = parse_params(&body)?;
    invoke_on(&state, p, function, params).await
}

#[utoipa::path(get, path = "/api/v1/providers/{id}/config", tag = "providers",
    params(("id" = String, Path, description = "Provider id")),
    responses(
        (status = 200, description = "Current provider configuration", body = Object),
        (status = 404, description = "Unknown provider", body = ApiError)))]
async fn get_provider_config(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let p = provider_or_404(&state, &id)?;
    Ok(Json(p.get_config().await))
}

#[utoipa::path(put, path = "/api/v1/providers/{id}/config", tag = "providers",
    params(("id" = String, Path, description = "Provider id")),
    request_body(content = Object, description = "Provider configuration object (see the provider's configSchema)"),
    responses(
        (status = 200, description = "Configuration applied and persisted", body = Object),
        (status = 400, description = "Invalid configuration", body = ApiError),
        (status = 404, description = "Unknown provider", body = ApiError),
        (status = 409, description = "Provider busy; try again", body = ApiError)))]
async fn put_provider_config(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _: Unlocked,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiFailure> {
    let p = provider_or_404(&state, &id)?;
    p.set_config(body.clone()).await.map_err(provider_error)?;
    state
        .settings
        .update(|s| {
            s.providers.configs.insert(id.clone(), body.clone());
        })
        .map_err(|e| fail(StatusCode::BAD_REQUEST, "invalid_settings", e.to_string()))?;
    Ok(Json(p.get_config().await))
}

#[utoipa::path(get, path = "/api/v1/providers/{id}/link", tag = "providers",
    params(("id" = String, Path, description = "Provider id")),
    responses(
        (status = 200, description = "Reachability of the physical link to the terminal. Cheap enough \
                                      to poll for a status light; never runs while a transaction owns \
                                      the provider.", body = LinkStatus),
        (status = 404, description = "Unknown provider", body = ApiError)))]
async fn get_provider_link(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<LinkStatus>, ApiFailure> {
    let p = provider_or_404(&state, &id)?;
    Ok(Json(p.probe_link().await))
}

async fn index() -> Json<Value> {
    Json(json!({
        "name": "Ledger POS API",
        "version": env!("CARGO_PKG_VERSION"),
        "docs": "/docs",
        "openapi": "/api-docs/openapi.json",
        "api": "/api/v1"
    }))
}

// ------------------------------------------------------------------ OpenAPI

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Ledger POS API",
        description = "Local REST bridge to payment POS terminals. Providers are self-describing: \
                       list them, read their function catalogs and parameter specs, then invoke \
                       functions with a JSON body. The `sandbox` provider simulates a terminal for \
                       development without hardware.\n\n\
                       Card-flow functions (purchase, balance, ...) block until the cardholder acts \
                       and can take up to two minutes — use generous HTTP timeouts.",
        version = env!("CARGO_PKG_VERSION"),
        license(name = "MIT")
    ),
    paths(
        health,
        system,
        get_settings,
        put_settings,
        activity,
        list_providers,
        get_active_provider,
        set_active_provider,
        get_provider,
        list_functions,
        get_function,
        invoke_function,
        invoke_on_active,
        get_provider_config,
        put_provider_config,
        get_provider_link,
        auth_status,
        auth_unlock,
        auth_lock,
    ),
    components(schemas(
        ApiError,
        HealthResponse,
        SystemInfo,
        ServerStatus,
        ProviderSummary,
        ProviderDetail,
        SetActiveRequest,
        SettingsUpdateResponse,
        Settings,
        crate::settings::ServerSettings,
        crate::settings::AppBehaviorSettings,
        crate::settings::ProvidersSettings,
        ActivityEntry,
        ProviderMetadata,
        ProviderStatus,
        LinkStatus,
        LinkState,
        AuthStatus,
        UnlockRequest,
        UnlockResponse,
        FunctionSpec,
        crate::providers::ParamSpec,
    )),
    tags(
        (name = "system", description = "Health, runtime info and activity"),
        (name = "settings", description = "Application settings (fully user-editable)"),
        (name = "providers", description = "Provider metadata, status, configuration and selection"),
        (name = "functions", description = "Function catalogs, specs and invocation"),
        (name = "security", description = "Application lock. When an administrator has set a password, \
                                           routes that change settings, the active provider or a provider's \
                                           configuration require `Authorization: Bearer <token>`. Reads and \
                                           function invocation stay open.")
    )
)]
pub struct ApiDoc;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/v1/health", get(health))
        .route("/api/v1/system", get(system))
        .route("/api/v1/settings", get(get_settings).put(put_settings))
        .route("/api/v1/activity", get(activity))
        .route("/api/v1/auth/status", get(auth_status))
        .route("/api/v1/auth/unlock", post(auth_unlock))
        .route("/api/v1/auth/lock", post(auth_lock))
        .route("/api/v1/providers", get(list_providers))
        .route(
            "/api/v1/providers/active",
            get(get_active_provider).put(set_active_provider),
        )
        .route(
            "/api/v1/providers/active/functions/{function}/invoke",
            post(invoke_on_active),
        )
        .route("/api/v1/providers/{id}", get(get_provider))
        .route(
            "/api/v1/providers/{id}/config",
            get(get_provider_config).put(put_provider_config),
        )
        .route("/api/v1/providers/{id}/link", get(get_provider_link))
        .route("/api/v1/providers/{id}/functions", get(list_functions))
        .route("/api/v1/providers/{id}/functions/{function}", get(get_function))
        .route(
            "/api/v1/providers/{id}/functions/{function}/invoke",
            post(invoke_function),
        )
        .merge(SwaggerUi::new("/docs").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::providers::{sandbox::SandboxProvider, ProviderRegistry};
    use crate::settings::SettingsStore;

    use crate::security::{hash_password, save_to, AdminConfig, AuthState};

    /// State backed by throwaway files, so tests never read or write the
    /// developer's real configuration or administrator file.
    fn state_with_password(password: Option<&str>) -> AppState {
        let dir = std::env::temp_dir().join(format!("ledger-pos-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let admin_file = dir.join("admin.json");
        if let Some(pw) = password {
            save_to(
                &admin_file,
                &AdminConfig {
                    password_hash: Some(hash_password(pw).unwrap()),
                    auto_lock_minutes: 15,
                },
            )
            .unwrap();
        }
        let settings = Arc::new(SettingsStore::at(dir.join("settings.json")));
        let registry = Arc::new(ProviderRegistry::new(vec![Arc::new(SandboxProvider::new(
            Value::Null,
        ))]));
        AppState::new(settings, registry, Arc::new(AuthState::new(admin_file)))
    }

    fn test_state() -> AppState {
        state_with_password(None)
    }

    async fn send(
        state: &AppState,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&b).unwrap()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let res = build_router(state.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 256 * 1024).await.unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    async fn unlock(state: &AppState, password: &str) -> String {
        let (status, body) = send(
            state,
            "POST",
            "/api/v1/auth/unlock",
            None,
            Some(json!({ "password": password })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "unlock failed: {body}");
        body["token"].as_str().expect("a token").to_string()
    }

    // ------------------------------------------------------------ auth gate

    /// Without a password configured nothing changes for anyone.
    #[tokio::test]
    async fn settings_can_be_written_when_no_password_is_set() {
        let state = test_state();
        let (_, current) = send(&state, "GET", "/api/v1/settings", None, None).await;
        let (status, _) = send(&state, "PUT", "/api/v1/settings", None, Some(current)).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn writing_settings_is_locked_without_a_token() {
        let state = state_with_password(Some("open sesame"));
        let (_, current) = send(&state, "GET", "/api/v1/settings", None, None).await;

        let (status, body) = send(&state, "PUT", "/api/v1/settings", None, Some(current)).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "locked");
    }

    #[tokio::test]
    async fn writing_settings_succeeds_with_a_token() {
        let state = state_with_password(Some("open sesame"));
        let token = unlock(&state, "open sesame").await;
        let (_, current) = send(&state, "GET", "/api/v1/settings", None, None).await;

        let (status, body) =
            send(&state, "PUT", "/api/v1/settings", Some(&token), Some(current)).await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
    }

    #[tokio::test]
    async fn switching_the_active_provider_is_locked() {
        let state = state_with_password(Some("open sesame"));
        let (status, _) = send(
            &state,
            "PUT",
            "/api/v1/providers/active",
            None,
            Some(json!({ "id": "sandbox" })),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn writing_provider_config_is_locked() {
        let state = state_with_password(Some("open sesame"));
        let (status, _) = send(
            &state,
            "PUT",
            "/api/v1/providers/sandbox/config",
            None,
            Some(json!({ "failRate": 0 })),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// The whole point: the till keeps taking payments while the app is locked.
    #[tokio::test]
    async fn invoking_a_terminal_function_stays_open_while_locked() {
        let state = state_with_password(Some("open sesame"));
        let (status, body) = send(
            &state,
            "POST",
            "/api/v1/providers/sandbox/functions/connectionTest/invoke",
            None,
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    }

    #[tokio::test]
    async fn reads_stay_open_while_locked() {
        let state = state_with_password(Some("open sesame"));
        for uri in [
            "/api/v1/health",
            "/api/v1/system",
            "/api/v1/settings",
            "/api/v1/providers",
            "/api/v1/activity",
        ] {
            let (status, _) = send(&state, "GET", uri, None, None).await;
            assert_eq!(status, StatusCode::OK, "{uri} should stay readable");
        }
    }

    // --------------------------------------------------------- auth endpoints

    #[tokio::test]
    async fn auth_status_reports_whether_a_password_is_set() {
        let open = test_state();
        let (status, body) = send(&open, "GET", "/api/v1/auth/status", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["passwordSet"], false);
        assert_eq!(body["locked"], false);

        let closed = state_with_password(Some("open sesame"));
        let (_, body) = send(&closed, "GET", "/api/v1/auth/status", None, None).await;
        assert_eq!(body["passwordSet"], true);
        assert_eq!(body["locked"], true);
        assert_eq!(body["autoLockMinutes"], 15);
    }

    #[tokio::test]
    async fn a_wrong_password_is_rejected() {
        let state = state_with_password(Some("open sesame"));
        let (status, body) = send(
            &state,
            "POST",
            "/api/v1/auth/unlock",
            None,
            Some(json!({ "password": "guess" })),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "invalid_password");
    }

    /// The password is set by the administrator on the machine, never over HTTP.
    #[tokio::test]
    async fn unlocking_is_refused_when_no_password_is_configured() {
        let state = test_state();
        let (status, body) = send(
            &state,
            "POST",
            "/api/v1/auth/unlock",
            None,
            Some(json!({ "password": "anything" })),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(body["code"], "no_password_set");
    }

    #[tokio::test]
    async fn locking_revokes_the_token() {
        let state = state_with_password(Some("open sesame"));
        let token = unlock(&state, "open sesame").await;

        let (status, _) = send(&state, "POST", "/api/v1/auth/lock", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);

        let (_, current) = send(&state, "GET", "/api/v1/settings", None, None).await;
        let (status, _) =
            send(&state, "PUT", "/api/v1/settings", Some(&token), Some(current)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "the token should be dead");
    }

    async fn body_json(res: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(res.into_body(), 256 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn get(uri: &str) -> (StatusCode, Value) {
        let res = build_router(test_state())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        (status, body_json(res).await)
    }

    #[tokio::test]
    async fn link_endpoint_reports_the_provider_link_state() {
        let (status, body) = get("/api/v1/providers/sandbox/link").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "notApplicable", "body: {body}");
    }

    #[tokio::test]
    async fn link_endpoint_404s_for_an_unknown_provider() {
        let (status, body) = get("/api/v1/providers/nope/link").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "unknown_provider");
    }

    /// A terminal that is switched off or has not opened its listener is a
    /// temporary reachability problem the caller can retry, not a protocol
    /// failure of the gateway.
    #[test]
    fn unreachable_terminal_maps_to_503_terminal_unreachable() {
        let f = provider_error(ProviderError::Unreachable(
            "terminal at 192.168.1.198:1197 refused the connection".into(),
        ));

        assert_eq!(f.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(f.1.code, "terminal_unreachable");
        assert!(f.1.error.contains("192.168.1.198:1197"), "message lost: {}", f.1.error);
    }

    /// The pre-existing classifications must not drift.
    #[test]
    fn execution_failures_stay_502() {
        let f = provider_error(ProviderError::Execution("boom".into()));
        assert_eq!(f.0, StatusCode::BAD_GATEWAY);
        assert_eq!(f.1.code, "execution_failed");
    }
}

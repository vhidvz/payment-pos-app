//! REST API routes + OpenAPI document.
//!
//! Everything is generated from the provider abstraction: provider metadata,
//! function catalogs, parameter specs and invocation all flow through the
//! generic `Provider` trait — no provider-specific route exists.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use crate::activity::ActivityEntry;
use crate::providers::{
    FunctionSpec, Provider, ProviderError, ProviderMetadata, ProviderStatus,
};
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
        FunctionSpec,
        crate::providers::ParamSpec,
    )),
    tags(
        (name = "system", description = "Health, runtime info and activity"),
        (name = "settings", description = "Application settings (fully user-editable)"),
        (name = "providers", description = "Provider metadata, status, configuration and selection"),
        (name = "functions", description = "Function catalogs, specs and invocation")
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
        .route("/api/v1/providers/{id}/functions", get(list_functions))
        .route("/api/v1/providers/{id}/functions/{function}", get(get_function))
        .route(
            "/api/v1/providers/{id}/functions/{function}/invoke",
            post(invoke_function),
        )
        .merge(SwaggerUi::new("/docs").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .with_state(state)
}

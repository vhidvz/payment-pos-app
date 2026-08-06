//! Embedded HTTP server: axum + OpenAPI (Swagger UI at /docs).
//!
//! Runs as a supervised task: binds the host/port from settings, serves until
//! the server settings change (then gracefully rebinds), and reports bind
//! failures through [`ServerStatus`] instead of crashing the app.

pub mod routes;

use std::sync::{Arc, RwLock};

use axum::http::{HeaderValue, Method};
use serde::Serialize;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use utoipa::ToSchema;

use crate::activity::ActivityLog;
use crate::providers::ProviderRegistry;
use crate::settings::{ServerSettings, SettingsStore};

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub running: bool,
    pub host: String,
    pub port: u16,
    /// Bind error, when not running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Default for ServerStatus {
    fn default() -> Self {
        Self {
            running: false,
            host: String::new(),
            port: 0,
            error: None,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<SettingsStore>,
    pub registry: Arc<ProviderRegistry>,
    pub activity: Arc<ActivityLog>,
    pub started_at: std::time::Instant,
    pub server_status: Arc<RwLock<ServerStatus>>,
}

impl AppState {
    pub fn new(settings: Arc<SettingsStore>, registry: Arc<ProviderRegistry>) -> Self {
        Self {
            settings,
            registry,
            activity: Arc::new(ActivityLog::default()),
            started_at: std::time::Instant::now(),
            server_status: Arc::new(RwLock::new(ServerStatus::default())),
        }
    }

    pub fn status(&self) -> ServerStatus {
        self.server_status.read().expect("status lock").clone()
    }

    fn set_status(&self, status: ServerStatus) {
        *self.server_status.write().expect("status lock") = status;
    }
}

fn cors_layer(server: &ServerSettings) -> CorsLayer {
    let layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS])
        .allow_headers(Any);
    if server.cors_allow_all {
        layer.allow_origin(Any)
    } else {
        let origins: Vec<HeaderValue> = server
            .cors_allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        layer.allow_origin(AllowOrigin::list(origins))
    }
}

/// Serve forever, rebinding whenever the server section of the settings changes.
///
/// Lockout protection: settings are edited *through* this API, so if freshly
/// changed server settings cannot bind, we fall back to the last address that
/// worked (reporting the failure in [`ServerStatus`]) instead of going dark —
/// the user can immediately correct the settings from the UI or a curl.
pub async fn run_server(state: AppState) {
    let mut rx = state.settings.subscribe();
    // The most recent server settings that successfully bound.
    let mut last_good: Option<ServerSettings> = None;

    loop {
        let desired = rx.borrow_and_update().server.clone();

        // Try the desired address first; fall back to last-good if it fails.
        let mut bound = None;
        let mut bind_error: Option<String> = None;
        for candidate in [Some(desired.clone()), last_good.clone()]
            .into_iter()
            .flatten()
        {
            let addr = format!("{}:{}", candidate.host, candidate.port);
            match tokio::net::TcpListener::bind(&addr).await {
                Ok(listener) => {
                    bound = Some((candidate, listener));
                    break;
                }
                Err(e) => {
                    let msg = format!("cannot bind {addr}: {e}");
                    tracing::error!("{msg}");
                    bind_error.get_or_insert(msg);
                }
            }
        }

        let Some((active, listener)) = bound else {
            state.set_status(ServerStatus {
                running: false,
                host: desired.host.clone(),
                port: desired.port,
                error: bind_error,
            });
            // Retry when settings change, or periodically in case the port frees up.
            tokio::select! {
                _ = rx.changed() => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            }
            continue;
        };

        last_good = Some(active.clone());
        let fallback_note = if active != desired {
            // Bound the old address because the new one failed — surface that.
            bind_error
        } else {
            None
        };
        tracing::info!(
            "REST API listening on http://{}:{} (docs at /docs)",
            active.host,
            active.port
        );
        state.set_status(ServerStatus {
            running: true,
            host: active.host.clone(),
            port: active.port,
            error: fallback_note,
        });

        let app = routes::build_router(state.clone()).layer(cors_layer(&active));

        let mut shutdown_rx = rx.clone();
        let baseline = desired.clone();
        let shutdown = async move {
            loop {
                if shutdown_rx.changed().await.is_err() {
                    // Settings store dropped — keep serving until process exit.
                    std::future::pending::<()>().await;
                }
                if shutdown_rx.borrow().server != baseline {
                    tracing::info!("server settings changed; restarting listener");
                    break;
                }
            }
        };

        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            tracing::error!("http server error: {e}");
        }
        state.set_status(ServerStatus {
            running: false,
            host: active.host.clone(),
            port: active.port,
            error: None,
        });
    }
}

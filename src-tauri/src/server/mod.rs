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
use crate::events::EventBus;
use crate::providers::ProviderRegistry;
use crate::security::AuthState;
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

/// Where `posd` serves right now — written once it binds, so the console probes (and points
/// its UI at) the real address, including the last-good one a failed rebind fell back to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Serving {
    pub pid: u32,
    pub host: String,
    pub port: u16,
    pub version: String,
}

/// `serving.json`, beside `settings.json`.
pub fn serving_path() -> std::path::PathBuf {
    crate::settings::settings_path().with_file_name("serving.json")
}

/// The published address, if the file is there and readable. Whether its `pid` still runs is
/// the reader's question.
pub fn read_serving(path: &std::path::Path) -> Option<Serving> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn publish(path: &std::path::Path, host: &str, port: u16) {
    let serving = Serving {
        pid: std::process::id(),
        host: host.to_string(),
        port,
        version: crate::version::VERSION.to_string(),
    };
    let bytes = serde_json::to_vec_pretty(&serving).expect("serving serializes");
    let meta = std::fs::metadata(path).ok();
    if let Err(e) = crate::fsutil::atomic_write(path, &bytes, meta.as_ref(), 0o644) {
        tracing::warn!("cannot write {} ({e}); the console probes the configured port instead", path.display());
    }
}

#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<SettingsStore>,
    pub registry: Arc<ProviderRegistry>,
    /// Lock state: the administrator's password file plus live sessions.
    pub auth: Arc<AuthState>,
    pub activity: Arc<ActivityLog>,
    /// Webhook events ([`crate::events`]). In memory unless the caller swaps in a
    /// persistent bus, which [`crate::build_state`] does for the real app.
    pub events: Arc<EventBus>,
    /// `state.json`: the last version that ran and the pending "updated" notice
    /// ([`crate::runstate`]). In memory unless [`crate::build_state`] opens the file.
    pub run_state: Arc<crate::runstate::StateStore>,
    /// Drain gate: every mutating request holds a ticket ([`crate::admission`]).
    pub admission: Arc<crate::admission::Admission>,
    pub started_at: std::time::Instant,
    pub server_status: Arc<RwLock<ServerStatus>>,
    /// Where to publish the bound address ([`Serving`]); `None` in tests.
    pub serving_file: Option<std::path::PathBuf>,
}

impl AppState {
    pub fn new(
        settings: Arc<SettingsStore>,
        registry: Arc<ProviderRegistry>,
        auth: Arc<AuthState>,
    ) -> Self {
        Self {
            settings,
            registry,
            auth,
            activity: Arc::new(ActivityLog::default()),
            events: Arc::new(EventBus::new(None)),
            run_state: Arc::new(crate::runstate::StateStore::in_memory()),
            admission: Arc::new(crate::admission::Admission::new()),
            started_at: std::time::Instant::now(),
            server_status: Arc::new(RwLock::new(ServerStatus::default())),
            serving_file: None,
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
        // The console's own origins are always answered (`CONSOLE_ORIGINS`), but never
        // written into the list: the security digest hashes the list as saved, so a
        // receiver that recorded it sees no change from this.
        let origins: Vec<HeaderValue> = crate::settings::CONSOLE_ORIGINS
            .iter()
            .copied()
            .chain(server.cors_allowed_origins.iter().map(String::as_str))
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
        if let Some(path) = &state.serving_file {
            let port = listener.local_addr().map(|a| a.port()).unwrap_or(active.port);
            publish(path, &active.host, port);
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The console probes, and points its UI at, where posd really serves — which is the
    /// last address that worked when a new one cannot be bound.
    #[tokio::test]
    async fn the_bound_address_is_published() {
        let dir = std::env::temp_dir().join(format!("ledger-serving-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let settings = Arc::new(SettingsStore::at(dir.join("settings.json")));
        settings.update(|s| s.server.port = port).unwrap();
        let registry = Arc::new(ProviderRegistry::new(vec![]));
        let auth = Arc::new(AuthState::new(dir.join("admin.json")));
        let mut state = AppState::new(settings, registry, auth);
        state.serving_file = Some(dir.join("serving.json"));
        tokio::spawn(run_server(state.clone()));
        let mut published = None;
        for _ in 0..100 {
            if let Some(s) = read_serving(&dir.join("serving.json")) {
                published = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let published = published.expect("serving.json written once bound");
        assert_eq!((published.host.as_str(), published.port), ("127.0.0.1", port));
        assert_eq!(published.pid, std::process::id());
        assert_eq!(published.version, crate::version::VERSION);
    }

    /// What `Access-Control-Allow-Origin` the layer answers for `origin`, if any.
    async fn allowed(server: &ServerSettings, origin: &str) -> Option<String> {
        use axum::{body::Body, http::Request, routing::get, Router};
        use tower::ServiceExt;
        let app = Router::new().route("/", get(|| async { "ok" })).layer(cors_layer(server));
        let res = app
            .oneshot(Request::builder().uri("/").header("origin", origin).body(Body::empty()).unwrap())
            .await
            .unwrap();
        res.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap().to_owned())
    }

    /// The console reaches posd from its own webview, so a list narrowed to one shop
    /// site must not cut the console off ("API offline" with posd healthy) — while
    /// every other origin stays exactly as the list says.
    #[tokio::test]
    async fn the_console_is_always_allowed() {
        let server = ServerSettings {
            cors_allowed_origins: vec!["http://localhost:3105".into()],
            cors_allow_all: false,
            ..ServerSettings::default()
        };
        for origin in crate::settings::CONSOLE_ORIGINS {
            assert_eq!(allowed(&server, origin).await.as_deref(), Some(origin), "{origin}");
        }
        assert_eq!(allowed(&server, "http://localhost:3105").await.as_deref(), Some("http://localhost:3105"));
        assert_eq!(allowed(&server, "http://evil.example").await, None);

        // The console's writes go through a preflight first.
        use axum::{body::Body, http::Request, routing::get, Router};
        use tower::ServiceExt;
        for (origin, expected) in [("tauri://localhost", Some("tauri://localhost")), ("http://evil.example", None)] {
            let app = Router::new().route("/", get(|| async { "ok" })).layer(cors_layer(&server));
            let req = Request::builder()
                .method("OPTIONS")
                .uri("/")
                .header("origin", origin)
                .header("access-control-request-method", "PUT")
                .body(Body::empty())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            let got = res.headers().get("access-control-allow-origin").map(|v| v.to_str().unwrap());
            assert_eq!(got, expected, "preflight from {origin}");
        }
    }
}

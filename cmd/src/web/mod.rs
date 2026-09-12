//! The web application: JSON API, the embedded single-page UI, the token-gated
//! download API, the run spawner and the scheduler — one process, one port.

pub mod api;
pub mod auth;
pub mod billing;
pub mod dispatch;
pub mod public_api;
pub mod download;
pub mod runner;
pub mod scheduler;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, Request, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rust_embed::RustEmbed;
use tokio::sync::{broadcast, Mutex};

use crate::store::Db;

#[derive(RustEmbed)]
#[folder = "../UI/web/dist"]
struct Assets;

/// One line of a run's output, fanned out to every SSE subscriber.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LogEvent {
    pub execution_id: i64,
    pub seq: i64,
}

pub struct AppState {
    pub db: Db,
    pub active: Mutex<HashMap<i64, runner::ActiveRun>>,
    pub start_gate: Mutex<()>,
    pub log_tx: broadcast::Sender<LogEvent>,
    pub dev: bool,
    pub open_signup: bool,
}

pub type App = Arc<AppState>;

/// Every JSON error the API returns, so handlers can `?` freely.
pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, axum::Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let msg = format!("{e:#}");
        // Anything that reads like a validation failure is the caller's; the
        // rest is ours and gets logged rather than echoed in detail.
        if msg.contains("not found") {
            ApiError(StatusCode::NOT_FOUND, msg)
        } else if msg.contains("already") || msg.contains("must") || msg.contains("required") || msg.contains("invalid") {
            ApiError(StatusCode::BAD_REQUEST, msg)
        } else {
            tracing::error!("{msg}");
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        anyhow::Error::from(e).into()
    }
}

pub fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
pub fn not_found(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}

/// The all-in-one server: UI, API and an in-process scheduler. This is what
/// `huntwell serve` and dev.sh run, where one process is the whole product.
pub async fn serve(db: Db, addr: &str) -> Result<()> {
    serve_app(db, addr, true).await
}

/// The `website` service: the same UI and API with no scheduler, because the
/// scheduling service owns firing. The website runs on every node behind the
/// edge load balancer, so anything periodic in here would run once per node.
pub async fn serve_website(db: Db, addr: &str) -> Result<()> {
    serve_app(db, addr, false).await
}

async fn serve_app(db: Db, addr: &str, with_scheduler: bool) -> Result<()> {
    // Runs left 'running' by a previous server are dead; say so before the
    // scheduler concludes those plans are busy. NOT in pool mode: pool runs
    // execute on remote pods and survive this process — the admin's heartbeat
    // reaper owns their staleness.
    if dispatch::mode() != dispatch::Mode::Pool {
        let stale = crate::store::abandon_stale_executions(&db).await?;
        if stale > 0 {
            tracing::warn!("marked {stale} run(s) from a previous server as failed");
        }
    }
    // Recomputing belongs to whoever fires. In the split the scheduling service
    // does it at startup; doing it here as well would repeat the same work once
    // per node, since the website runs on all of them.
    if with_scheduler {
        let n = crate::store::refresh_all_schedules(&db).await?;
        tracing::info!("recomputed {n} schedule(s)");
    }
    let _ = crate::store::delete_expired_sessions(&db).await;

    let (log_tx, _) = broadcast::channel(4096);
    let state: App = Arc::new(AppState {
        db,
        active: Mutex::new(HashMap::new()),
        start_gate: Mutex::new(()),
        log_tx,
        dev: crate::config::is_dev(),
        open_signup: crate::config::open_signup(),
    });
    if with_scheduler {
        // The all-in-one process is also the scheduler and, unless drafts are
        // queued for a planning service, the drafter. Same pid on those
        // heartbeats, so the admin can see they are this process, not a replica.
        crate::bus::start_heartbeat_as("scheduling");
        if !api::draft_queued() {
            crate::bus::start_heartbeat_as("planning");
        }
        scheduler::spawn(state.clone());
    }
    api::listen_metered_runs(state.clone());

    let app = Router::new()
        .nest("/api", api::router())
        // The public, versioned surface. `/api` is the UI's own and changes
        // with it; `/v1` is the promise.
        .nest("/v1", public_api::router())
        .nest("/dl", download::router())
        .route("/healthz", get(|| async { "ok" }))
        .fallback(get(static_handler))
        .layer(middleware::from_fn_with_state(state.clone(), harden_headers))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("bind {addr}"))?;
    tracing::info!("huntwell listening on http://{addr}{}", if state.dev { " (dev mode)" } else { "" });
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(shutdown_signal(state.clone()))
        .await?;
    Ok(())
}

/// One microservice in the k3d split. Each mounts only its slice of the API and


async fn shutdown_signal(state: App) {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down — stopping active runs");
    runner::stop_all(&state).await;
}

async fn harden_headers(State(state): State<App>, req: Request<Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    // The UI is self-contained apart from Google Fonts; the dev server proxies
    // through, so the same policy holds there.
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
             font-src 'self' https://fonts.gstatic.com data:; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'",
        ),
    );
    if state.dev {
        h.insert("access-control-allow-credentials", HeaderValue::from_static("true"));
    }
    res
}

/// Serves the embedded UI, falling back to index.html for client-side routes.
async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => asset_response(path, file.data.into_owned()),
        None => match Assets::get("index.html") {
            Some(file) => asset_response("index.html", file.data.into_owned()),
            None => (
                StatusCode::NOT_FOUND,
                "UI bundle not built — run `npm run build` in UI/web (or ./dev.sh) and rebuild",
            )
                .into_response(),
        },
    }
}

fn asset_response(path: &str, data: Vec<u8>) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap()),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
        ],
        data,
    )
        .into_response()
}

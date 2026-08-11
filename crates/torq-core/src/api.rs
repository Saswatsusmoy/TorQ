//! REST + SSE API for the daemon.
//!
//! Bound to 127.0.0.1 only; every route except the middleware passes requires
//! `Authorization: Bearer <token>` (token lives in config.toml so local
//! clients can read it).

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::stream::{Stream, StreamExt};
use librqbit::api::TorrentIdOrHash;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;

use crate::daemon::{Daemon, Event, TorrentView};
use crate::VERSION;

#[derive(Clone)]
pub struct AppState {
    pub daemon: Arc<Daemon>,
    pub sources: Arc<torq_sources::Registry>,
    pub client: reqwest::Client,
    auth_token: String,
}

pub fn router(
    daemon: Arc<Daemon>,
    auth_token: String,
    sources: Arc<torq_sources::Registry>,
    client: reqwest::Client,
) -> Router {
    let state = Arc::new(AppState {
        daemon,
        sources,
        client,
        auth_token,
    });
    Router::new()
        .route("/health", get(health))
        .route("/torrents", get(list_torrents).post(add_torrent))
        .route("/torrents/{id}", delete(remove_torrent))
        .route("/torrents/{id}/pause", post(pause_torrent))
        .route("/torrents/{id}/resume", post(resume_torrent))
        .route("/search", get(search))
        .route("/events", get(events))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_auth,
        ))
        .with_state(state)
}

async fn require_auth(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let authed = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|token| constant_time_eq(token.as_bytes(), state.auth_token.as_bytes()))
        .unwrap_or(false);
    if authed {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response()
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// -- handlers ---------------------------------------------------------------

#[derive(Serialize)]
struct Health {
    version: &'static str,
    torrents: usize,
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Health> {
    Json(Health {
        version: VERSION,
        torrents: state.daemon.views().len(),
    })
}

async fn list_torrents(State(state): State<Arc<AppState>>) -> Json<Vec<TorrentView>> {
    Json(state.daemon.views())
}

#[derive(Deserialize)]
struct SearchReq {
    q: String,
    /// Comma-separated source ids; empty = all.
    #[serde(default)]
    sources: Option<String>,
}

/// Aggregated search across enabled sources, deduped by infohash. Failing
/// sources are reported in `offline`, never fatal.
async fn search(
    State(state): State<Arc<AppState>>,
    Query(req): Query<SearchReq>,
) -> Json<torq_sources::SearchReport> {
    let only = req
        .sources
        .as_deref()
        .map(|s| s.split(',').map(str::to_string).collect::<Vec<_>>());
    let report = torq_sources::aggregate::search_all(
        &state.sources.sources,
        &state.client,
        &req.q,
        only.as_deref(),
    )
    .await;
    Json(report)
}

#[derive(Deserialize)]
struct AddReq {
    magnet: String,
    #[serde(default)]
    paused: bool,
}

async fn add_torrent(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddReq>,
) -> Result<Json<TorrentView>, ApiError> {
    let view = state.daemon.add_magnet(&req.magnet, req.paused).await?;
    Ok(Json(view))
}

#[derive(Deserialize, Default)]
struct RemoveReq {
    #[serde(default, deserialize_with = "deserialize_bool_flag")]
    delete_files: bool,
}

/// serde_urlencoded only parses `true`/`false` for bools; scripts and curl
/// users naturally write `?delete_files=1`, so accept both spellings.
fn deserialize_bool_flag<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    match s.as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(serde::de::Error::custom(format!(
            "expected true/false/1/0, got {other:?}"
        ))),
    }
}

async fn remove_torrent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(req): Query<RemoveReq>,
) -> Result<StatusCode, ApiError> {
    let parsed = TorrentIdOrHash::parse(&id)?;
    state.daemon.remove(parsed, req.delete_files).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn pause_torrent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.daemon.pause(TorrentIdOrHash::parse(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn resume_torrent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.daemon.resume(TorrentIdOrHash::parse(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let stream = BroadcastStream::new(state.daemon.subscribe()).map(|item| {
        let data = match item {
            Ok(Event::TorrentFailed { id, error }) => {
                serde_json::json!({"type": "torrent_failed", "id": id, "error": error}).to_string()
            }
            Ok(ev) => serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into()),
            Err(_) => "{}".into(), // lagged behind; client should re-poll /torrents
        };
        Ok(SseEvent::default().event("torq").data(data))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// -- errors ------------------------------------------------------------------

pub enum ApiError {
    NotFound(String),
    BadRequest(String),
    Internal(anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound(m) => (StatusCode::NOT_FOUND, m).into_response(),
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
            Self::Internal(e) => {
                tracing::error!("api error: {e:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let msg = e.to_string();
        if msg.contains("not found") || msg.contains("not managed") {
            Self::NotFound(msg)
        } else if msg.contains("not a valid magnet") || msg.contains("failed to parse") {
            Self::BadRequest(msg)
        } else {
            Self::Internal(e)
        }
    }
}

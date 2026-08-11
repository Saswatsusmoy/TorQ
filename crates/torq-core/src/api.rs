//! REST + SSE API for the daemon.
//!
//! Bound to 127.0.0.1 only; every route except the middleware passes requires
//! `Authorization: Bearer <token>` (token lives in config.toml so local
//! clients can read it).

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use futures::stream::{Stream, StreamExt};
use librqbit::api::TorrentIdOrHash;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::io::ReaderStream;

use crate::VERSION;
use crate::daemon::{Daemon, Event, TorrentView};

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
        .route("/torrents/{id}/files", get(torrent_files))
        .route("/torrents/{id}/stream/{file_id}", get(stream_file))
        .route("/search", get(search))
        .route("/rss", get(list_rss).post(add_rss))
        .route("/rss/{id}", delete(remove_rss))
        .route("/library", get(library_status).post(library_scan))
        .route("/config/limits", patch(set_limits))
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

#[derive(Serialize)]
struct FileInfo {
    id: usize,
    name: String,
    length: u64,
    included: bool,
}

async fn torrent_files(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<FileInfo>>, ApiError> {
    let details = state
        .daemon
        .engine()
        .api()
        .api_torrent_details(TorrentIdOrHash::parse(&id)?)?;
    let files = details
        .files
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, f)| FileInfo {
            id: i,
            name: f.components.join("/"),
            length: f.length,
            included: f.included,
        })
        .collect();
    Ok(Json(files))
}

/// HTTP range streaming of a torrent file, works mid-download: librqbit's
/// `FileStream` reads pieces on demand (32MB lookahead), so a player can start
/// before the file completes — mpv/VLC over this endpoint are the target.
async fn stream_file(
    State(state): State<Arc<AppState>>,
    Path((id, file_id)): Path<(String, usize)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let parsed = TorrentIdOrHash::parse(&id)?;
    let api = state.daemon.engine().api();
    let details = api.api_torrent_details(parsed)?;
    let files = details.files.as_deref().unwrap_or_default();
    let file = files
        .get(file_id)
        .ok_or_else(|| ApiError::NotFound(format!("file {file_id} not found")))?;
    let total = file.length;

    let mut stream = api.api_stream(parsed, file_id)?;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| parse_range(r, total));
    let (status, start, end) = match range {
        Some((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e),
        None => (StatusCode::OK, 0, total.saturating_sub(1)),
    };
    if start > 0 {
        use tokio::io::AsyncSeekExt;
        stream.seek(std::io::SeekFrom::Start(start)).await?;
    }
    let len = end - start + 1;

    let mut headers = HeaderMap::new();
    headers.insert("Accept-Ranges", HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(mime_for(&file.name)),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")).expect("valid header"),
        );
    }
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&len.to_string()).expect("valid header"),
    );

    let body = Body::from_stream(ReaderStream::with_capacity(stream.take(len), 64 * 1024));
    Ok((status, headers, body).into_response())
}

/// Parse a single-range `bytes=` header; returns (start, end), inclusive.
fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    if total == 0 {
        return None;
    }
    let spec = header.strip_prefix("bytes=")?;
    let (start_str, end_str) = spec.split_once('-')?;
    if start_str.is_empty() {
        // Suffix range: last N bytes.
        let n = end_str.parse::<u64>().ok()?;
        if n == 0 {
            return None;
        }
        let start = total.saturating_sub(n);
        return Some((start, total - 1));
    }
    let start = start_str.parse::<u64>().ok()?;
    if start >= total {
        return None;
    }
    let end = if end_str.is_empty() {
        total - 1
    } else {
        end_str.parse::<u64>().ok()?.min(total - 1)
    };
    (end >= start).then_some((start, end))
}

fn mime_for(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "ts" => "video/mp2t",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        _ => "application/octet-stream",
    }
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
    #[serde(default)]
    magnet: String,
    #[serde(default)]
    paused: bool,
    /// Base64-encoded .torrent bytes (mutually exclusive with magnet).
    #[serde(default)]
    torrent_b64: Option<String>,
}

async fn add_torrent(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddReq>,
) -> Result<Json<TorrentView>, ApiError> {
    let view = match req.torrent_b64 {
        Some(b64) => {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .map_err(|e| ApiError::BadRequest(format!("invalid torrent_b64: {e}")))?;
            state.daemon.add_torrent_bytes(bytes, req.paused).await?
        }
        None if req.magnet.trim().is_empty() => {
            return Err(ApiError::BadRequest(
                "provide a magnet or torrent_b64".into(),
            ));
        }
        None => state.daemon.add_magnet(&req.magnet, req.paused).await?,
    };
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

async fn list_rss(State(state): State<Arc<AppState>>) -> Json<Vec<crate::rss::Subscription>> {
    Json(state.daemon.rss.list())
}

#[derive(Deserialize)]
struct AddRssReq {
    url: String,
    #[serde(default)]
    title_re: Option<String>,
    #[serde(default)]
    min_size: Option<u64>,
    #[serde(default)]
    max_size: Option<u64>,
    #[serde(default = "default_sub_interval")]
    interval_secs: u64,
}

fn default_sub_interval() -> u64 {
    300
}

async fn add_rss(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddRssReq>,
) -> Result<Json<crate::rss::Subscription>, ApiError> {
    let sub = state.daemon.rss.add(
        &req.url,
        req.title_re,
        req.min_size,
        req.max_size,
        req.interval_secs,
    )?;
    Ok(Json(sub))
}

async fn remove_rss(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u64>,
) -> Result<StatusCode, ApiError> {
    if state.daemon.rss.remove(id) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("subscription {id} not found")))
    }
}

#[derive(Serialize)]
struct LibraryStatus {
    indexed: usize,
    dirs: Vec<PathBuf>,
}

async fn library_status(State(state): State<Arc<AppState>>) -> Json<LibraryStatus> {
    Json(LibraryStatus {
        indexed: state.daemon.library.count(),
        dirs: state.daemon.library.dirs(),
    })
}

/// Rescan library dirs; returns the number of torrents indexed.
async fn library_scan(State(state): State<Arc<AppState>>) -> Result<Json<LibraryStatus>, ApiError> {
    state.daemon.library.scan()?;
    Ok(Json(LibraryStatus {
        indexed: state.daemon.library.count(),
        dirs: state.daemon.library.dirs(),
    }))
}

#[derive(Deserialize)]
struct LimitsReq {
    upload_bps: Option<u32>,
    download_bps: Option<u32>,
}

/// Apply rate limits live (None clears the limit). Persists to config so the
/// daemon restarts with them.
async fn set_limits(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LimitsReq>,
) -> Result<StatusCode, ApiError> {
    state
        .daemon
        .engine()
        .set_limits(req.upload_bps, req.download_bps);
    let mut config = crate::config::Config::load()?;
    config.upload_bps = req.upload_bps;
    config.download_bps = req.download_bps;
    config.save()?;
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

impl From<librqbit::ApiError> for ApiError {
    fn from(e: librqbit::ApiError) -> Self {
        let msg = e.to_string();
        if msg.contains("not found") {
            Self::NotFound(msg)
        } else {
            Self::Internal(anyhow::anyhow!(msg))
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self::Internal(anyhow::anyhow!(e))
    }
}

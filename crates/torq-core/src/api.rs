//! REST + SSE API for the daemon.
//!
//! Bound to 127.0.0.1 only; routes require `Authorization: Bearer <token>`
//! (token lives in config.toml so local clients can read it), except the
//! stream route, which also accepts the short-lived capability token that
//! `/play` embeds in the URL — real players can't send headers, so the URL
//! itself is the ticket.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    api_port: u16,
    auth_token: String,
    /// Capability tokens minted by `/play`, keyed to their issue time. The
    /// stream route accepts one of these (in the URL) in place of the API
    /// bearer header, since players can't send headers.
    stream_tokens: Arc<Mutex<HashMap<String, Instant>>>,
}

/// How long a stream URL stays valid. Long enough for a player to pause and
/// reconnect mid-session; `/play` sweeps expired entries as it mints new
/// ones, so the map stays small.
const STREAM_TOKEN_TTL: Duration = Duration::from_secs(60 * 60);

pub fn router(
    daemon: Arc<Daemon>,
    auth_token: String,
    sources: Arc<torq_sources::Registry>,
    client: reqwest::Client,
    api_port: u16,
) -> Router {
    let state = Arc::new(AppState {
        daemon,
        sources,
        client,
        api_port,
        auth_token,
        stream_tokens: Arc::new(Mutex::new(HashMap::new())),
    });
    Router::new()
        .route("/health", get(health))
        .route("/torrents", get(list_torrents).post(add_torrent))
        .route("/torrents/{id}", delete(remove_torrent))
        .route("/torrents/{id}/pause", post(pause_torrent))
        .route("/torrents/{id}/resume", post(resume_torrent))
        .route("/torrents/{id}/files", get(torrent_files))
        .route("/torrents/{id}/play", get(play_file))
        .route("/torrents/{id}/stream/{file_id}", get(stream_file))
        .route("/search", get(search))
        .route("/rss", get(list_rss).post(add_rss))
        .route("/rss/{id}", delete(remove_rss))
        .route("/library", get(library_status).post(library_scan))
        .route("/config", get(get_config))
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
    let header_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|token| constant_time_eq(token.as_bytes(), state.auth_token.as_bytes()))
        .unwrap_or(false);
    // The stream route is also reachable with the capability token `/play`
    // embeds in the URL — players (VLC, IINA, mpv) can't send headers.
    let token_ok = req
        .uri()
        .query()
        .and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("token="))
                .map(|t| stream_token_valid(&state.stream_tokens, t))
        })
        .unwrap_or(false);
    if header_ok || token_ok {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response()
    }
}

fn stream_token_valid(map: &Mutex<HashMap<String, Instant>>, token: &str) -> bool {
    map.lock()
        .ok()
        .and_then(|m| m.get(token).copied())
        .is_some_and(|issued| issued.elapsed() < STREAM_TOKEN_TTL)
}

/// Fresh capability token for a stream URL (16 random bytes, hex).
fn new_stream_token() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("os rng");
    b.iter().map(|x| format!("{x:02x}")).collect()
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
struct ConfigInfo {
    /// Concurrent transfer slots; torrents beyond this wait in queue.
    max_active: usize,
}

async fn get_config(State(state): State<Arc<AppState>>) -> Json<ConfigInfo> {
    Json(ConfigInfo {
        max_active: state.daemon.max_active(),
    })
}

#[derive(Serialize, Debug, PartialEq)]
struct FileInfo {
    id: usize,
    name: String,
    length: u64,
    included: bool,
}

fn file_list(details: &librqbit::api::TorrentDetailsResponse) -> Vec<FileInfo> {
    details
        .files
        .as_deref()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, f)| FileInfo {
            id: i,
            name: f.components.join("/"),
            length: f.length,
            included: f.included,
        })
        .collect()
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
    Ok(Json(file_list(&details)))
}

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "webm", "avi", "mov", "m4v", "ts", "wmv", "flv",
];

/// Largest video file, else the largest file overall — what a player wants.
fn pick_play_file(files: &[FileInfo]) -> Option<&FileInfo> {
    let is_video = |f: &FileInfo| {
        f.name
            .rsplit('.')
            .next()
            .is_some_and(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
    };
    files
        .iter()
        .filter(|f| is_video(f))
        .max_by_key(|f| f.length)
        .or_else(|| files.iter().max_by_key(|f| f.length))
}

#[derive(Serialize)]
struct PlayResponse {
    url: String,
    name: String,
    file_id: usize,
    length: u64,
}

/// Resolve the playable stream URL for a torrent: the largest video file
/// (fallback: largest file), served by the range endpoint. One implementation
/// shared by `torq play` and the TUI's `P` key.
async fn play_file(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<PlayResponse>, ApiError> {
    let details = state
        .daemon
        .engine()
        .api()
        .api_torrent_details(TorrentIdOrHash::parse(&id)?)?;
    let files = file_list(&details);
    let file =
        pick_play_file(&files).ok_or_else(|| ApiError::NotFound("torrent has no files".into()))?;
    let mut tokens = state.stream_tokens.lock().expect("stream tokens");
    let now = Instant::now();
    tokens.retain(|_, issued| now.duration_since(*issued) < STREAM_TOKEN_TTL);
    let token = new_stream_token();
    tokens.insert(token.clone(), now);
    drop(tokens);
    let url = format!(
        "http://127.0.0.1:{}/torrents/{id}/stream/{}?token={token}",
        state.api_port, file.id
    );
    Ok(Json(PlayResponse {
        url,
        name: file.name.clone(),
        file_id: file.id,
        length: file.length,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: usize, name: &str, length: u64) -> FileInfo {
        FileInfo {
            id,
            name: name.into(),
            length,
            included: true,
        }
    }

    #[test]
    fn stream_tokens_expire_and_reject_unknown() {
        let map = Mutex::new(HashMap::new());
        assert!(!stream_token_valid(&map, "nope"));
        map.lock().unwrap().insert("fresh".into(), Instant::now());
        assert!(stream_token_valid(&map, "fresh"));
        map.lock()
            .unwrap()
            .insert("stale".into(), Instant::now() - STREAM_TOKEN_TTL - Duration::from_secs(1));
        assert!(!stream_token_valid(&map, "stale"));
        // A fresh token survives alongside the stale one.
        assert!(stream_token_valid(&map, "fresh"));
    }

    #[test]
    fn play_picks_largest_video_else_largest_file() {
        let files = vec![
            file(0, "cover.jpg", 500_000),
            file(1, "movie.mkv", 5_000_000_000),
            file(2, "sample.mp4", 200_000_000),
        ];
        assert_eq!(pick_play_file(&files).unwrap().id, 1);
        // No video: largest file wins.
        let only_audio = vec![file(0, "song.mp3", 10_000_000), file(1, "notes.txt", 1)];
        assert_eq!(pick_play_file(&only_audio).unwrap().id, 0);
        assert_eq!(pick_play_file(&[]), None);
    }
}

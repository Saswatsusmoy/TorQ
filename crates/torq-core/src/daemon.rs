//! Download queue, daemon state, and event broadcast.
//!
//! The queue is torlink's model: a fixed number of active slots; torrents
//! beyond the cap are engine-paused and auto-promoted as slots free. Status is
//! derived per torrent: user pause, engine pause (queued), error, completed.
//! Our own metadata (user_paused, added_at) persists to `queue.json`; the
//! engine session persists the torrents themselves.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use librqbit::api::{ApiTorrentListOpts, TorrentDetailsResponse, TorrentIdOrHash};
use librqbit::{AddTorrentResponse, TorrentStats, TorrentStatsState};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::engine::Engine;
use crate::rss::Subscriptions;

/// How many torrents download at once (torlink parity; tunable later).
pub const DEFAULT_MAX_ACTIVE: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Downloading,
    Queued,
    Paused,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentView {
    pub id: usize,
    pub info_hash: String,
    pub name: String,
    pub status: Status,
    /// 0..=1, 0 while metadata is still being fetched.
    pub progress: f32,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    pub upload_mbps: Option<f32>,
    pub download_mbps: Option<f32>,
    pub peers: usize,
    pub error: Option<String>,
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    TorrentAdded { id: usize },
    TorrentUpdated { id: usize },
    TorrentCompleted { id: usize },
    TorrentFailed { id: usize, error: String },
    TorrentRemoved { id: usize },
}

/// Per-torrent daemon metadata, keyed by info hash (stable across restarts,
/// unlike engine torrent ids). Only `user_paused`/`queued`/`added_at` persist.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    user_paused: bool,
    /// Waiting for an active slot (over the cap at add time).
    #[serde(default)]
    queued: bool,
    added_at: i64,
    #[serde(skip)]
    last_status: Option<Status>,
}

impl Default for Meta {
    fn default() -> Self {
        Self {
            user_paused: false,
            queued: false,
            added_at: now_secs(),
            last_status: None,
        }
    }
}

pub struct Daemon {
    engine: Arc<Engine>,
    max_active: usize,
    state_dir: PathBuf,
    meta: Mutex<HashMap<String, Meta>>,
    events: broadcast::Sender<Event>,
    /// RSS subscriptions; polled by a background task.
    pub rss: Arc<Subscriptions>,
}

impl Daemon {
    /// Start the daemon: hydrate metadata for restored torrents, promote any
    /// queued into free slots, and spawn the transition + RSS poll ticks.
    pub async fn start(config: &Config, engine: Arc<Engine>) -> Result<Arc<Self>> {
        let (events, _) = broadcast::channel(512);
        let rss = Subscriptions::load(config.state_dir.join("subscriptions.json"));
        let daemon = Arc::new(Self {
            engine,
            max_active: DEFAULT_MAX_ACTIVE,
            state_dir: config.state_dir.clone(),
            meta: Mutex::new(HashMap::new()),
            events,
            rss,
        });

        daemon.load_meta();
        // Metadata for torrents restored by the session (no prior meta file).
        {
            let mut meta = daemon.meta.lock();
            let resp = daemon
                .engine
                .api()
                .api_torrent_list_ext(ApiTorrentListOpts { with_stats: true });
            for t in &resp.torrents {
                meta.entry(t.info_hash.clone()).or_default();
            }
        }
        daemon.save_meta();
        daemon.try_promote().await;

        tokio::spawn(tick_loop(daemon.clone()));
        tokio::spawn(rss_poll_loop(daemon.clone(), config.socks_proxy.clone()));
        Ok(daemon)
    }

    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    // -- mutations ----------------------------------------------------------

    pub async fn add_magnet(&self, magnet: &str, paused: bool) -> Result<TorrentView> {
        let magnet = magnet.trim();
        let resp = self.engine.add_magnet(magnet).await?;
        self.finish_add(resp, paused).await
    }

    pub async fn add_torrent_bytes(&self, bytes: Vec<u8>, paused: bool) -> Result<TorrentView> {
        let resp = self.engine.add_torrent_bytes(bytes).await?;
        self.finish_add(resp, paused).await
    }

    async fn finish_add(&self, resp: AddTorrentResponse, paused: bool) -> Result<TorrentView> {
        let (id, info_hash) = match &resp {
            AddTorrentResponse::Added(id, h) | AddTorrentResponse::AlreadyManaged(id, h) => {
                (*id, h.info_hash().as_string())
            }
            AddTorrentResponse::ListOnly(_) => anyhow::bail!("torrent added in list-only mode"),
        };

        // Record intent only; librqbit refuses to pause a torrent that is still
        // initializing, so the reconcile tick applies pause/queue once the
        // torrent is ready. An explicit `paused` add reads back as "paused"
        // immediately via the meta flag. Re-adding an existing torrent must
        // never clobber the user's pause state.
        //
        // NOTE: active_count() takes the meta lock (via views()), so it must
        // run before we hold the guard — parking_lot is not reentrant.
        let is_new = matches!(resp, AddTorrentResponse::Added(_, _));
        let queued = is_new && !paused && self.active_count() > self.max_active;
        if queued {
            debug!(id, "over active cap, queueing");
        }
        {
            let mut meta = self.meta.lock();
            let entry = meta.entry(info_hash.clone()).or_default();
            if is_new {
                entry.user_paused = paused;
                entry.queued = queued;
                entry.last_status = None; // force a transition broadcast on next tick
            }
        }

        self.save_meta();
        let _ = self.events.send(Event::TorrentAdded { id });
        debug!(id, "finish_add: returning view");
        self.view(id).context("torrent disappeared after add")
    }

    pub async fn pause(&self, id: TorrentIdOrHash) -> Result<()> {
        let (id_num, hash) = self.locate(&id)?;
        self.meta
            .lock()
            .get_mut(&hash)
            .expect("meta exists")
            .user_paused = true;
        self.save_meta();
        // Best-effort: pausing a still-initializing torrent fails in librqbit;
        // the reconcile tick retries until it takes.
        if let Err(e) = self.engine.pause(TorrentIdOrHash::Id(id_num)).await {
            debug!(id = id_num, "deferred pause not yet possible: {e}");
        }
        let _ = self.events.send(Event::TorrentUpdated { id: id_num });
        Ok(())
    }

    pub async fn resume(&self, id: TorrentIdOrHash) -> Result<()> {
        let (id_num, hash) = self.locate(&id)?;
        {
            let mut meta = self.meta.lock();
            let entry = meta.get_mut(&hash).expect("meta exists");
            entry.user_paused = false;
            entry.queued = false;
        }
        self.save_meta();
        self.try_promote().await;
        let _ = self.events.send(Event::TorrentUpdated { id: id_num });
        Ok(())
    }

    pub async fn remove(&self, id: TorrentIdOrHash, delete_files: bool) -> Result<()> {
        let (id_num, hash) = self.locate(&id)?;
        self.engine
            .remove(TorrentIdOrHash::Id(id_num), delete_files)
            .await?;
        self.meta.lock().remove(&hash);
        self.save_meta();
        let _ = self.events.send(Event::TorrentRemoved { id: id_num });
        self.try_promote().await;
        Ok(())
    }

    // -- reads ---------------------------------------------------------------

    pub fn views(&self) -> Vec<TorrentView> {
        let resp = self
            .engine
            .api()
            .api_torrent_list_ext(ApiTorrentListOpts { with_stats: true });
        let meta = self.meta.lock();
        resp.torrents
            .iter()
            .filter_map(|t| {
                let m = meta.get(&t.info_hash).cloned().unwrap_or_default();
                view_from(t, &m)
            })
            .collect()
    }

    pub fn view(&self, id: usize) -> Option<TorrentView> {
        self.views().into_iter().find(|v| v.id == id)
    }

    /// Resolve a torrent by numeric id or info hash to (id, info_hash).
    fn locate(&self, id: &TorrentIdOrHash) -> Result<(usize, String)> {
        let handle = self.engine.api().mgr_handle(*id).map_err(|e| {
            anyhow::anyhow!(if e.to_string().contains("not found") {
                "torrent not found"
            } else {
                "torrent lookup failed"
            })
        })?;
        Ok((handle.id(), handle.info_hash().as_string()))
    }

    fn active_count(&self) -> usize {
        self.views()
            .iter()
            .filter(|v| v.status == Status::Downloading)
            .count()
    }

    // -- queue promotion ------------------------------------------------------

    /// Resume queued torrents into free slots, oldest first.
    async fn try_promote(&self) {
        let active = self.active_count();
        if active >= self.max_active {
            return;
        }
        let mut queued: Vec<TorrentView> = self
            .views()
            .into_iter()
            .filter(|v| v.status == Status::Queued)
            .collect();
        queued.sort_by_key(|v| v.added_at);
        for v in queued.into_iter().take(self.max_active - active) {
            if let Err(e) = self.engine.resume(TorrentIdOrHash::Id(v.id)).await {
                warn!(id = v.id, "promote failed: {e:#}");
            } else {
                if let Some(entry) = self.meta.lock().get_mut(&v.info_hash) {
                    entry.queued = false;
                }
                info!(id = v.id, "promoted from queue");
            }
        }
        self.save_meta();
    }

    // -- persistence ----------------------------------------------------------

    fn meta_file(&self) -> PathBuf {
        self.state_dir.join("queue.json")
    }

    fn load_meta(&self) {
        let path = self.meta_file();
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return;
        };
        match serde_json::from_str::<HashMap<String, Meta>>(&raw) {
            Ok(loaded) => {
                *self.meta.lock() = loaded;
            }
            Err(e) => warn!(path = %path.display(), "ignoring unparsable queue.json: {e}"),
        }
    }

    fn save_meta(&self) {
        let map = self.meta.lock().clone();
        let raw = match serde_json::to_vec(&map) {
            Ok(v) => v,
            Err(e) => {
                warn!("queue.json serialization failed: {e}");
                return;
            }
        };
        let path = self.meta_file();
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, raw).and_then(|()| std::fs::rename(&tmp, &path)) {
            warn!(path = %path.display(), "queue.json write failed: {e:#}");
        }
    }

    // -- transition tick --------------------------------------------------------

    /// One pass over session state: detect status transitions, broadcast them,
    /// promote completions. Runs every second; does nothing when nothing changed.
    async fn reconcile(&self) {
        let resp = self
            .engine
            .api()
            .api_torrent_list_ext(ApiTorrentListOpts { with_stats: true });
        let mut completed = Vec::new();
        let mut failed = Vec::new();
        let mut updated = Vec::new();

        {
            let mut meta = self.meta.lock();
            for t in &resp.torrents {
                let Some(stats) = t.stats.as_ref() else {
                    continue;
                };
                let Some(id) = t.id else { continue };
                let entry = meta.entry(t.info_hash.clone()).or_default();
                let status = derive_status(stats, entry);
                let prev = entry.last_status;
                entry.last_status = Some(status);
                match (prev, status) {
                    (Some(_), Status::Completed) => completed.push(id),
                    (Some(_), Status::Failed) => {
                        failed.push((id, stats.error.clone().unwrap_or_default()))
                    }
                    (Some(p), s) if p != s => updated.push((id, status)),
                    _ => {}
                }
            }
        }

        for id in &completed {
            let _ = self.events.send(Event::TorrentCompleted { id: *id });
        }
        for (id, error) in &failed {
            let _ = self.events.send(Event::TorrentFailed {
                id: *id,
                error: error.clone(),
            });
        }
        for (id, _) in &updated {
            let _ = self.events.send(Event::TorrentUpdated { id: *id });
        }

        // Enforce recorded intent the engine hasn't applied yet: pausing a
        // torrent mid-initialization fails in librqbit, so retry each tick.
        for t in &resp.torrents {
            let Some(id) = t.id else { continue };
            let Some(stats) = t.stats.as_ref() else {
                continue;
            };
            let meta = self.meta.lock().get(&t.info_hash).cloned();
            let Some(meta) = meta else { continue };
            if (meta.user_paused || meta.queued)
                && !matches!(
                    stats.state,
                    TorrentStatsState::Paused | TorrentStatsState::Error
                )
            {
                if let Err(e) = self.engine.pause(TorrentIdOrHash::Id(id)).await {
                    debug!(id, "deferred pause not yet possible: {e}");
                }
            }
        }

        // Fill any free slots (also covers torrents restored paused by the
        // session with no recorded intent).
        self.try_promote().await;

        if !completed.is_empty() {
            debug!("{} torrent(s) completed", completed.len());
        }
    }
}

async fn tick_loop(daemon: Arc<Daemon>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        daemon.reconcile().await;
    }
}

/// Poll due RSS subscriptions every 30s; each subscription staggers itself.
async fn rss_poll_loop(daemon: Arc<Daemon>, socks_proxy: Option<String>) {
    let Ok(client) = torq_sources::types::http_client(socks_proxy.as_deref()) else {
        warn!("rss polling disabled: no HTTP client");
        return;
    };
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;
        daemon.rss.poll_due(&client, &daemon).await;
    }
}

/// Status precedence: error → completed → user-paused → engine-paused (queued)
/// → downloading. A completed torrent that the user paused stays completed.
fn derive_status(stats: &TorrentStats, meta: &Meta) -> Status {
    if stats.error.is_some() && !stats.finished {
        return Status::Failed;
    }
    if stats.finished {
        return Status::Completed;
    }
    if meta.user_paused {
        return Status::Paused;
    }
    if meta.queued {
        return Status::Queued;
    }
    match stats.state {
        TorrentStatsState::Paused => Status::Queued,
        _ => Status::Downloading,
    }
}

fn view_from(details: &TorrentDetailsResponse, meta: &Meta) -> Option<TorrentView> {
    let id = details.id?;
    let stats = details.stats.as_ref()?;
    let (download_mbps, upload_mbps, peers) = match &stats.live {
        Some(live) => (
            Some(live.download_speed.mbps as f32),
            Some(live.upload_speed.mbps as f32),
            live.snapshot.peer_stats.live,
        ),
        None => (None, None, 0),
    };
    Some(TorrentView {
        id,
        info_hash: details.info_hash.clone(),
        name: details
            .name
            .clone()
            .unwrap_or_else(|| details.info_hash.clone()),
        status: derive_status(stats, meta),
        progress: if stats.total_bytes > 0 {
            stats.progress_bytes as f32 / stats.total_bytes as f32
        } else {
            0.0
        },
        total_bytes: stats.total_bytes,
        downloaded_bytes: stats.progress_bytes,
        upload_mbps,
        download_mbps,
        peers,
        error: stats.error.clone(),
        added_at: meta.added_at,
    })
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(state: TorrentStatsState) -> TorrentStats {
        TorrentStats {
            state,
            file_progress: vec![],
            error: None,
            progress_bytes: 0,
            uploaded_bytes: 0,
            total_bytes: 100,
            finished: false,
            live: None,
        }
    }

    fn meta(user_paused: bool) -> Meta {
        Meta {
            user_paused,
            queued: false,
            added_at: 1,
            last_status: None,
        }
    }

    fn meta_queued() -> Meta {
        Meta {
            user_paused: false,
            queued: true,
            added_at: 1,
            last_status: None,
        }
    }

    #[test]
    fn status_precedence() {
        assert_eq!(
            derive_status(&stats(TorrentStatsState::Live), &meta(false)),
            Status::Downloading
        );
        assert_eq!(
            derive_status(&stats(TorrentStatsState::Initializing), &meta(false)),
            Status::Downloading
        );
        // engine-paused = queued (waiting for a slot)
        assert_eq!(
            derive_status(&stats(TorrentStatsState::Paused), &meta(false)),
            Status::Queued
        );
        // user-paused wins over engine pause
        assert_eq!(
            derive_status(&stats(TorrentStatsState::Paused), &meta(true)),
            Status::Paused
        );
        assert_eq!(
            derive_status(&stats(TorrentStatsState::Live), &meta(true)),
            Status::Paused
        );
        // over-cap intent reads as queued even while the engine catches up
        assert_eq!(
            derive_status(&stats(TorrentStatsState::Live), &meta_queued()),
            Status::Queued
        );
        // user pause wins over queued intent
        let mut m = meta_queued();
        m.user_paused = true;
        assert_eq!(
            derive_status(&stats(TorrentStatsState::Live), &m),
            Status::Paused
        );
    }

    #[test]
    fn finished_beats_paused_and_error_clears() {
        let mut s = stats(TorrentStatsState::Paused);
        s.finished = true;
        assert_eq!(derive_status(&s, &meta(true)), Status::Completed);

        s.error = Some("boom".into());
        s.finished = false;
        assert_eq!(derive_status(&s, &meta(false)), Status::Failed);
    }

    #[test]
    fn view_maps_progress_and_speeds() {
        let mut s = stats(TorrentStatsState::Live);
        s.progress_bytes = 50;
        s.total_bytes = 200;
        let details = TorrentDetailsResponse {
            id: Some(7),
            info_hash: "abc".into(),
            name: Some("x".into()),
            output_folder: "/tmp".into(),
            files: None,
            stats: Some(s),
        };
        let v = view_from(&details, &meta(false)).unwrap();
        assert_eq!(v.id, 7);
        assert_eq!(v.status, Status::Downloading);
        assert!((v.progress - 0.25).abs() < f32::EPSILON);
        assert_eq!(v.downloaded_bytes, 50);
    }
}

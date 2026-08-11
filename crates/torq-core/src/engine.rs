//! Thin wrapper over the librqbit engine session.
//!
//! The daemon's only job here: construct the session from [`Config`], expose
//! add/remove/pause/resume, and hand out the `Api` facade (serializable DTOs)
//! for the REST layer. Queue semantics and persistence-on-top live in the
//! daemon module (next phase).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use librqbit::api::{Api, TorrentIdOrHash};
use librqbit::limits::LimitsConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, Session, SessionOptions,
    SessionPersistenceConfig,
};
use url::Url;

/// Default add options: resume semantics. librqbit refuses to open existing
/// files unless `overwrite` is set; a downloader must be able to resume an
/// interrupted download (the piece check validates what's on disk).
fn default_add_options() -> AddTorrentOptions {
    AddTorrentOptions {
        overwrite: true,
        ..Default::default()
    }
}

use crate::config::Config;

pub struct Engine {
    api: Api,
    session: Arc<Session>,
    pub download_dir: PathBuf,
}

impl Engine {
    /// Start a librqbit session shaped by `config`, restoring any persisted
    /// torrents from the previous run.
    pub async fn start(config: &Config) -> Result<Arc<Self>> {
        let opts = SessionOptions {
            fastresume: true,
            dht_config: Some(librqbit::dht::PersistentDhtConfig {
                dump_interval: Some(std::time::Duration::from_secs(300)),
                config_filename: Some(config.state_dir.join("dht.json")),
            }),
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(config.state_dir.join("session")),
            }),
            socks_proxy_url: config.socks_proxy.clone(),
            ratelimits: LimitsConfig {
                upload_bps: config.upload_bps.and_then(NonZeroU32::new),
                download_bps: config.download_bps.and_then(NonZeroU32::new),
            },
            trackers: config
                .trackers
                .iter()
                .filter_map(|t| Url::parse(t).ok())
                .collect(),
            ..Default::default()
        };

        let session = Session::new_with_opts(config.download_dir.clone(), opts)
            .await
            .context("starting librqbit session")?;

        Ok(Arc::new(Self {
            api: Api::new(session.clone(), None),
            session,
            download_dir: config.download_dir.clone(),
        }))
    }

    pub fn api(&self) -> &Api {
        &self.api
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// Add a magnet link or bare 40-char infohash. The caller matches on
    /// [`AddTorrentResponse`] for the torrent id/handle. Bounded: a magnet
    /// whose metadata never resolves (no reachable peers) errors after 30s
    /// instead of hanging the request forever (librqbit has no resolve timeout).
    pub async fn add_magnet(&self, magnet: &str) -> Result<AddTorrentResponse> {
        let fut = self.session.add_torrent(
            AddTorrent::from_url(magnet.to_string()),
            Some(default_add_options()),
        );
        tokio::time::timeout(Duration::from_secs(30), fut)
            .await
            .context("metadata resolution timed out (no reachable peers?)")?
            .context("adding torrent")
    }

    /// Apply session-wide rate limits live (None = unlimited).
    pub fn set_limits(&self, upload_bps: Option<u32>, download_bps: Option<u32>) {
        self.session
            .ratelimits
            .set_upload_bps(upload_bps.and_then(NonZeroU32::new));
        self.session
            .ratelimits
            .set_download_bps(download_bps.and_then(NonZeroU32::new));
    }

    /// Add a magnet, downloading into `output_folder` (cross-seed: point it at
    /// existing library data so the piece check finds it instead of fetching).
    pub async fn add_magnet_with_output(
        &self,
        magnet: &str,
        output_folder: PathBuf,
    ) -> Result<AddTorrentResponse> {
        let opts = AddTorrentOptions {
            output_folder: Some(output_folder.to_string_lossy().into_owned()),
            ..default_add_options()
        };
        let fut = self
            .session
            .add_torrent(AddTorrent::from_url(magnet.to_string()), Some(opts));
        tokio::time::timeout(Duration::from_secs(30), fut)
            .await
            .context("metadata resolution timed out (no reachable peers?)")?
            .context("adding torrent")
    }

    /// Add an in-memory .torrent file (bytes from disk, HTTP, or watch folder).
    pub async fn add_torrent_bytes(&self, bytes: Vec<u8>) -> Result<AddTorrentResponse> {
        let fut = self
            .session
            .add_torrent(AddTorrent::from_bytes(bytes), Some(default_add_options()));
        tokio::time::timeout(Duration::from_secs(30), fut)
            .await
            .context("metadata resolution timed out")?
            .context("adding torrent")
    }

    /// Delete a torrent. `delete_files` removes its files from disk.
    pub async fn remove(&self, id: TorrentIdOrHash, delete_files: bool) -> Result<()> {
        self.session
            .delete(id, delete_files)
            .await
            .context("removing torrent")
    }

    pub async fn pause(&self, id: TorrentIdOrHash) -> Result<()> {
        let handle = self.api.mgr_handle(id)?;
        self.session.pause(&handle).await.context("pausing torrent")
    }

    pub async fn resume(&self, id: TorrentIdOrHash) -> Result<()> {
        let handle = self.api.mgr_handle(id)?;
        self.session
            .unpause(&handle)
            .await
            .context("resuming torrent")
    }

    /// Snapshot of every torrent in the session (id, name, hashes, state).
    pub fn list(&self) -> librqbit::api::TorrentListResponse {
        self.api.api_torrent_list()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_id_accepts_hash_and_numeric_id() {
        let hash = "cab507494d02ebb1178b38f2e9d7be299c86b862";
        assert!(matches!(
            TorrentIdOrHash::parse(hash).unwrap(),
            TorrentIdOrHash::Hash(_)
        ));
        let id = TorrentIdOrHash::parse("3").unwrap();
        assert!(matches!(id, TorrentIdOrHash::Id(_)));
        assert!(TorrentIdOrHash::parse("not-an-id").is_err());
    }
}

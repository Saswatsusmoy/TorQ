//! Shared contract for search sources: what a source is and what it returns.
//!
//! Mirrors torlink's `sources/types.ts` so the ten adapters port 1:1.

use serde::{Deserialize, Serialize};

/// Stable identifier for a source. Also the plugin directory name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceId {
    Fitgirl,
    Yts,
    Eztv,
    Nyaa,
    Subsplease,
    TpbMovies,
    TpbTv,
    X1337Movies,
    X1337Tv,
    Bittorrented,
}

/// The category tabs a source feeds. A general index can feed several.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceGroup {
    Games,
    Movies,
    Tv,
    Anime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentResult {
    pub info_hash: String,
    pub name: String,
    pub size_bytes: u64,
    pub seeders: u32,
    pub leechers: u32,
    pub num_files: Option<u32>,
    pub source: SourceId,
    pub magnet: String,
    pub added: Option<i64>,
}

/// A search backend. `search` runs concurrently for every enabled source; a
/// failing source returns an error the aggregator surfaces as "offline" rather
/// than failing the whole search (torlink behavior).
// All implementors live in this crate; auto-trait bounds are not a concern.
#[allow(async_fn_in_trait)]
pub trait Source: Send + Sync {
    fn id(&self) -> SourceId;
    fn label(&self) -> &'static str;
    fn groups(&self) -> &'static [SourceGroup];
    fn homepage(&self) -> &'static str;
    /// True when the source reports real swarm counts. When false, `seeders: 0`
    /// means "unknown", not "dead" — the alive-only filter must never drop rows.
    fn reports_health(&self) -> bool;

    async fn search(&self, query: &str, client: &reqwest::Client) -> anyhow::Result<Vec<TorrentResult>>;
}

/// Shared HTTP client: browser-ish UA plus optional SOCKS proxy, reused across
/// all sources so TLS sessions and connection pools are shared.
pub fn http_client(socks_proxy: Option<&str>) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("torq/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(20));
    if let Some(proxy) = socks_proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
}

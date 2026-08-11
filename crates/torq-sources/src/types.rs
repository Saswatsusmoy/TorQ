//! Shared contract for search sources: what a source is and what it returns.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Source id (kebab-case); plugin ids are arbitrary strings.
    pub source: String,
    pub magnet: String,
    pub added: Option<i64>,
}

/// A search backend. Runs concurrently per source; a failing source reports an
/// error the aggregator surfaces as "offline" instead of failing the search.
#[async_trait::async_trait]
pub trait Source: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> &str;
    fn groups(&self) -> &[SourceGroup];
    fn homepage(&self) -> &str;
    /// True when the source reports real swarm counts. When false, `seeders: 0`
    /// means "unknown", not "dead" — the alive-only filter must never drop rows.
    fn reports_health(&self) -> bool;

    async fn search(
        &self,
        query: &str,
        client: &reqwest::Client,
    ) -> anyhow::Result<Vec<TorrentResult>>;
}

/// Shared HTTP client: browser-ish UA plus optional SOCKS proxy, reused across
/// all sources so TLS sessions and connection pools are shared.
pub fn http_client(socks_proxy: Option<&str>) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/125.0 Safari/537.36")
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(20));
    if let Some(proxy) = socks_proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
}

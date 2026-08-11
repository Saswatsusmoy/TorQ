//! Aggregation: run enabled sources concurrently, dedupe by infohash (keep
//! the row with the most seeders), sort by seeders, and report which sources
//! were unreachable so the UI can say "X is offline" without failing.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::types::{Source, TorrentResult};

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchReport {
    pub results: Vec<TorrentResult>,
    /// Source ids that errored (offline / blocked / returned garbage).
    pub offline: Vec<String>,
}

pub async fn search_all(
    sources: &[Arc<dyn Source>],
    client: &reqwest::Client,
    query: &str,
    only: Option<&[String]>,
) -> SearchReport {
    let futures = sources
        .iter()
        .filter(|s| only.is_none_or(|ids| ids.iter().any(|id| id == s.id())))
        .map(|s| async move { (s, s.search(query, client).await) });
    let outcomes = futures::future::join_all(futures).await;

    let mut offline = Vec::new();
    let mut by_hash: HashMap<String, TorrentResult> = HashMap::new();
    for (s, res) in outcomes {
        match res {
            Ok(rows) => {
                for mut r in rows {
                    match by_hash.entry(r.info_hash.clone()) {
                        Entry::Occupied(mut e) => {
                            // Keep the row with the most seeders; swap instead
                            // of cloning so the loser's strings are dropped
                            // rather than duplicated.
                            if r.seeders > e.get().seeders {
                                std::mem::swap(e.get_mut(), &mut r);
                            }
                        }
                        Entry::Vacant(v) => {
                            v.insert(r);
                        }
                    }
                }
            }
            Err(_) => offline.push(s.id().to_string()),
        }
    }
    let mut results: Vec<TorrentResult> = by_hash.into_values().collect();
    results.sort_by(|a, b| {
        b.seeders
            .cmp(&a.seeders)
            .then_with(|| b.added.cmp(&a.added))
    });
    SearchReport { results, offline }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SourceGroup;

    struct Fake {
        id: &'static str,
        rows: Vec<TorrentResult>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl Source for Fake {
        fn id(&self) -> &str {
            self.id
        }
        fn label(&self) -> &str {
            self.id
        }
        fn groups(&self) -> &[SourceGroup] {
            &[]
        }
        fn homepage(&self) -> &str {
            ""
        }
        fn reports_health(&self) -> bool {
            true
        }
        async fn search(
            &self,
            _q: &str,
            _c: &reqwest::Client,
        ) -> anyhow::Result<Vec<TorrentResult>> {
            if self.fail {
                anyhow::bail!("down")
            }
            Ok(self.rows.clone())
        }
    }

    fn row(hash: &str, seeders: u32, source: &str) -> TorrentResult {
        TorrentResult {
            info_hash: hash.into(),
            name: hash.into(),
            size_bytes: 0,
            seeders,
            leechers: 0,
            num_files: None,
            source: source.into(),
            magnet: format!("magnet:?xt=urn:btih:{hash}"),
            added: None,
        }
    }

    #[tokio::test]
    async fn dedupes_by_hash_keeping_max_seeders() {
        let a = Arc::new(Fake {
            id: "a",
            fail: false,
            rows: vec![row("h1", 10, "a")],
        }) as Arc<dyn Source>;
        let b = Arc::new(Fake {
            id: "b",
            fail: false,
            rows: vec![row("h1", 99, "b"), row("h2", 5, "b")],
        }) as Arc<dyn Source>;
        let c = Arc::new(Fake {
            id: "c",
            fail: true,
            rows: vec![],
        }) as Arc<dyn Source>;
        let client = reqwest::Client::new();
        let report = search_all(&[a, b, c], &client, "x", None).await;
        assert_eq!(report.offline, vec!["c"]);
        let hashes: Vec<&str> = report
            .results
            .iter()
            .map(|r| r.info_hash.as_str())
            .collect();
        assert_eq!(hashes, vec!["h1", "h2"]);
        assert_eq!(report.results[0].source, "b"); // the 99-seeder row won
    }
}

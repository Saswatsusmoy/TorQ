//! SubsPlease: JSON object of releases, each with per-resolution magnets.
//! Picks the best available resolution (1080 > 720 > 480).

use anyhow::Context;
use serde_json::Value;

use crate::types::{Source, SourceGroup, TorrentResult};
use crate::util::fetch_with_failover;

const HOST: &str = "https://subsplease.org";
const RES_PREFERENCE: &[&str] = &["1080", "720", "480"];

pub struct SubsPlease;

fn pick_best(downloads: &[Value]) -> Option<&Value> {
    RES_PREFERENCE
        .iter()
        .find_map(|res| downloads.iter().find(|d| d["res"] == *res))
        .or_else(|| downloads.iter().find(|d| d["magnet"].as_str().is_some()))
}

#[async_trait::async_trait]
impl Source for SubsPlease {
    fn id(&self) -> &str {
        "subsplease"
    }
    fn label(&self) -> &str {
        "SubsPlease"
    }
    fn groups(&self) -> &[SourceGroup] {
        &[SourceGroup::Anime]
    }
    fn homepage(&self) -> &str {
        "https://subsplease.org"
    }
    fn reports_health(&self) -> bool {
        false
    }

    async fn search(
        &self,
        query: &str,
        client: &reqwest::Client,
    ) -> anyhow::Result<Vec<TorrentResult>> {
        let q = query.trim();
        let path = if q.is_empty() {
            "/api/?f=latest&tz=UTC".to_string()
        } else {
            format!("/api/?f=search&s={}&tz=UTC", q.replace(' ', "+"))
        };
        let body = fetch_with_failover(client, &[HOST.to_string()], &path).await?;
        let json: Value = serde_json::from_str(&body).context("parsing SubsPlease JSON")?;
        let Some(obj) = json.as_object() else {
            return Ok(vec![]);
        };

        let mut out = Vec::new();
        for entry in obj.values() {
            let show = entry["show"].as_str().unwrap_or("Unknown");
            let ep = entry["episode"]
                .as_str()
                .map(|e| format!(" - {e}"))
                .unwrap_or_default();
            let downloads = entry["downloads"].as_array().cloned().unwrap_or_default();
            let Some(dl) = pick_best(&downloads) else {
                continue;
            };
            let Some(magnet) = dl["magnet"].as_str() else {
                continue;
            };
            let Some(raw_hash) = magnet
                .split("urn:btih:")
                .nth(1)
                .and_then(|s| s.split('&').next())
            else {
                continue;
            };
            let Some(info_hash) = crate::util::canonicalize_hash(raw_hash) else {
                continue;
            };
            let res = dl["res"].as_str().unwrap_or("?");
            out.push(TorrentResult {
                info_hash,
                name: format!("{show}{ep} [{res}p]"),
                size_bytes: magnet
                    .split("xl=")
                    .nth(1)
                    .and_then(|s| s.split('&').next())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                seeders: 0,
                leechers: 0,
                num_files: None,
                source: "subsplease".into(),
                magnet: magnet.to_string(),
                added: None,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_best_resolution() {
        let downloads = serde_json::json!([
            {"res": "480", "magnet": "m:1"},
            {"res": "1080", "magnet": "m:2"},
            {"res": "720", "magnet": "m:3"}
        ]);
        let arr = downloads.as_array().unwrap();
        assert_eq!(pick_best(arr).unwrap()["res"], "1080");
    }

    #[test]
    fn extracts_hash_and_size_from_magnet() {
        let magnet = "magnet:?xt=urn:btih:cab507494d02ebb1178b38f2e9d7be299c86b862&xl=123456";
        let hash = magnet
            .split("urn:btih:")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let size: u64 = magnet
            .split("xl=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(hash, "cab507494d02ebb1178b38f2e9d7be299c86b862");
        assert_eq!(size, 123456);
    }
}

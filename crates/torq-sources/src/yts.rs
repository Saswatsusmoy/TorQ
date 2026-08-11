//! YTS: JSON API whose response nests one movie → several quality torrents.
//! Too nested for the flat runner; this is the pure mapping (~40 lines).

use anyhow::Context;
use serde_json::Value;

use crate::types::{Source, SourceGroup, TorrentResult};
use crate::util::{build_magnet, fetch_with_failover};

const HOSTS: &[&str] = &["https://yts.mx", "https://yts.am", "https://yts.rs"];

pub struct Yts;

#[async_trait::async_trait]
impl Source for Yts {
    fn id(&self) -> &str {
        "yts"
    }
    fn label(&self) -> &str {
        "YTS"
    }
    fn groups(&self) -> &[SourceGroup] {
        &[SourceGroup::Movies]
    }
    fn homepage(&self) -> &str {
        "https://yts.mx"
    }
    fn reports_health(&self) -> bool {
        true
    }

    async fn search(
        &self,
        query: &str,
        client: &reqwest::Client,
    ) -> anyhow::Result<Vec<TorrentResult>> {
        let q = query.trim();
        let path = if q.is_empty() {
            "/api/v2/list_movies.json?limit=50&sort_by=date_added".to_string()
        } else {
            format!(
                "/api/v2/list_movies.json?limit=50&query_term={}",
                urlencoding(q)
            )
        };
        let hosts: Vec<String> = HOSTS.iter().map(|s| s.to_string()).collect();
        let body = fetch_with_failover(client, &hosts, &path).await?;
        let json: Value = serde_json::from_str(&body).context("parsing YTS JSON")?;
        let movies = json
            .pointer("/data/movies")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut out = Vec::new();
        for movie in movies {
            let base = movie["title_long"]
                .as_str()
                .or_else(|| movie["title"].as_str())
                .unwrap_or("Unknown")
                .to_string();
            let added = movie["date_uploaded_unix"].as_i64();
            for t in movie["torrents"].as_array().cloned().unwrap_or_default() {
                let Some(hash) = t["hash"].as_str().map(|h| h.to_lowercase()) else {
                    continue;
                };
                let tag = [t["quality"].as_str(), t["type"].as_str()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                let name = if tag.is_empty() {
                    base.clone()
                } else {
                    format!("{base} [{tag}]")
                };
                let magnet = build_magnet(&hash, &name);
                out.push(TorrentResult {
                    info_hash: hash,
                    name,
                    size_bytes: t["size_bytes"].as_u64().unwrap_or(0),
                    seeders: t["seeds"].as_u64().unwrap_or(0) as u32,
                    leechers: t["peers"].as_u64().unwrap_or(0) as u32,
                    num_files: None,
                    source: "yts".into(),
                    magnet,
                    added,
                });
            }
        }
        Ok(out)
    }
}

fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_nested_torrents_with_tags() {
        let json: Value = serde_json::json!({"data": {"movies": [{
            "title_long": "Inception",
            "date_uploaded_unix": 1700000000,
            "torrents": [
                {"hash": "CAB507494D02EBB1178B38F2E9D7BE299C86B862", "quality": "1080p", "type": "bluray", "size_bytes": 100, "seeds": 5, "peers": 1},
                {"hash": "aabb494d02ebb1178b38f2e9d7be299c86b861", "quality": "720p", "type": "web", "size_bytes": 50, "seeds": 2, "peers": 0}
            ]
        }]}});
        let movies = json
            .pointer("/data/movies")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        let mut out = Vec::new();
        for movie in movies {
            let base = movie["title_long"].as_str().unwrap().to_string();
            for t in movie["torrents"].as_array().unwrap() {
                let hash = t["hash"].as_str().unwrap().to_lowercase();
                let tag = [t["quality"].as_str(), t["type"].as_str()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push((format!("{base} [{tag}]"), hash));
            }
        }
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            (
                "Inception [1080p bluray]".to_string(),
                "cab507494d02ebb1178b38f2e9d7be299c86b862".to_string()
            )
        );
        assert_eq!(out[1].0, "Inception [720p web]");
    }
}

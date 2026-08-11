//! Declarative RSS/Atom source: Nyaa (with its namespaced fields) and FitGirl
//! (WordPress feed with magnet links in `<link>`). Plugins reuse this struct.

use std::sync::Arc;

use serde::Deserialize;

use crate::types::{Source, SourceGroup, TorrentResult};
use crate::util::{build_magnet, canonicalize_hash, fetch_with_failover, parse_size};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RssDef {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub groups: Vec<SourceGroup>,
    pub homepage: String,
    /// Defaults to true; FitGirl's feed has no swarm data.
    #[serde(default = "yes")]
    pub reports_health: bool,
    pub hosts: Vec<String>,
    /// Feed path when browsing (no query).
    pub path: String,
    /// Path used when searching (FitGirl: "/"), with `search_param` + extras.
    #[serde(default)]
    pub search_path: Option<String>,
    #[serde(default)]
    pub search_param: Option<String>,
    #[serde(default)]
    pub search_extra: Vec<(String, String)>,
    /// Nyaa: infohash lives in a namespaced extension, e.g. "nyaa:infoHash".
    #[serde(default)]
    pub hash_field: Option<String>,
    #[serde(default)]
    pub size_field: Option<String>,
    #[serde(default)]
    pub seeders_field: Option<String>,
    #[serde(default)]
    pub leechers_field: Option<String>,
}

fn yes() -> bool {
    true
}

pub struct RssSource {
    pub def: RssDef,
}

impl RssSource {
    pub fn new(def: RssDef) -> Arc<Self> {
        Arc::new(Self { def })
    }
}

#[async_trait::async_trait]
impl Source for RssSource {
    fn id(&self) -> &str {
        &self.def.id
    }
    fn label(&self) -> &str {
        &self.def.label
    }
    fn groups(&self) -> &[SourceGroup] {
        &self.def.groups
    }
    fn homepage(&self) -> &str {
        &self.def.homepage
    }
    fn reports_health(&self) -> bool {
        self.def.reports_health
    }

    async fn search(
        &self,
        query: &str,
        client: &reqwest::Client,
    ) -> anyhow::Result<Vec<TorrentResult>> {
        let q = query.trim();
        let def = &self.def;
        let path = if q.is_empty() {
            def.path.clone()
        } else {
            let mut params: Vec<(String, String)> = def.search_extra.clone();
            if let Some(p) = &def.search_param {
                params.push((p.clone(), q.to_string()));
            }
            let qs = params
                .iter()
                .map(|(k, v)| format!("{k}={}", urlencoding(v)))
                .collect::<Vec<_>>()
                .join("&");
            let base = def.search_path.as_deref().unwrap_or(&def.path);
            if qs.is_empty() {
                base.to_string()
            } else {
                format!("{base}?{qs}")
            }
        };

        let xml = fetch_with_failover(client, &def.hosts, &path).await?;
        Ok(self.parse(&xml))
    }
}

impl RssSource {
    /// Parse a feed body into results. Public so RSS subscriptions reuse the
    /// exact same item handling (magnets from link/description/content,
    /// namespaced infohash extensions, size/seed fields).
    pub fn parse(&self, xml: &str) -> Vec<TorrentResult> {
        let def = &self.def;
        let Ok(channel) = rss::Channel::read_from(xml.as_bytes()) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(channel.items().len());
        for item in channel.items() {
            // Magnet first: from the item link, else from its description or
            // content HTML (WordPress feeds embed magnets there).
            let magnet = magnet_from(item);
            let hash = match &def.hash_field {
                Some(f) => extension(item, f),
                None => magnet
                    .as_deref()
                    .and_then(extract_hash)
                    .map(str::to_string)
                    .or_else(|| extension_hash(item)),
            };
            let Some(hash) = hash.and_then(|h| canonicalize_hash(&h)) else {
                continue;
            };
            let name = item.title().unwrap_or(&hash).to_string();
            let magnet = magnet.unwrap_or_else(|| build_magnet(&hash, &name));
            out.push(TorrentResult {
                info_hash: hash,
                name,
                size_bytes: def
                    .size_field
                    .as_ref()
                    .and_then(|f| extension(item, f))
                    .map(|s| parse_size(&s))
                    .unwrap_or(0),
                seeders: def
                    .seeders_field
                    .as_ref()
                    .and_then(|f| extension(item, f))
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0),
                leechers: def
                    .leechers_field
                    .as_ref()
                    .and_then(|f| extension(item, f))
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0),
                num_files: None,
                source: def.id.clone(),
                magnet,
                added: None,
            });
        }
        out
    }
}

/// Any namespaced extension whose value canonicalizes to an infohash
/// (e.g. `nyaa:infoHash`) — lets generic subscriptions work on Nyaa-style
/// feeds without a per-feed config.
fn extension_hash(item: &rss::Item) -> Option<String> {
    item.extensions()
        .values()
        .flat_map(|ns| ns.values())
        .flatten()
        .find_map(|e| e.value().and_then(canonicalize_hash))
}

/// The item's magnet: from its `<link>` if that is a magnet, else the first
/// magnet href inside its description or content HTML (WordPress feeds embed
/// magnets in `<content:encoded>`).
fn magnet_from(item: &rss::Item) -> Option<String> {
    item.link()
        .filter(|l| l.starts_with("magnet:"))
        .map(str::to_string)
        .or_else(|| item.description().and_then(find_magnet))
        .or_else(|| item.content().and_then(find_magnet))
}

/// First `magnet:?xt=urn:btih:…` href in an HTML block (attribute-terminated,
/// with common entity unescaping).
fn find_magnet(s: &str) -> Option<String> {
    let start = s.find("magnet:?xt=urn:btih:")?;
    let rest = &s[start..];
    let end = rest.find(['"', '<']).unwrap_or(rest.len());
    Some(rest[..end].replace("&amp;", "&"))
}

/// Namespaced extension value: `extension(item, "nyaa:infoHash")` looks up the
/// `nyaa` namespace's `infoHash` extension (rss crate flattens `ns:name`).
fn extension(item: &rss::Item, field: &str) -> Option<String> {
    let (ns, name) = field.split_once(':')?;
    item.extensions()
        .get(ns)?
        .get(name)?
        .first()
        .and_then(|e| e.value())
        .map(str::to_string)
}

fn extract_hash(magnet: &str) -> Option<&str> {
    let xt = magnet.split("urn:btih:").nth(1)?;
    xt.split('&').next()
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
    fn extracts_hash_from_magnet() {
        assert_eq!(
            extract_hash("magnet:?xt=urn:btih:CAB507494D02EBB1178B38F2E9D7BE299C86B862&dn=x"),
            Some("CAB507494D02EBB1178B38F2E9D7BE299C86B862")
        );
    }

    #[test]
    fn wordpress_style_feed_extracts_magnet_from_description() {
        let xml = r#"<?xml version="1.0"?><rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel><item>
          <title>Game Repack v1.0</title>
          <link>https://site.example/game-repack-v1-0/</link>
          <description><![CDATA[<p>Download: <a href="magnet:?xt=urn:btih:CAB507494D02EBB1178B38F2E9D7BE299C86B862&amp;dn=game">magnet</a></p>]]></description>
          <content:encoded><![CDATA[<a href="magnet:?xt=urn:btih:CAB507494D02EBB1178B38F2E9D7BE299C86B862&amp;dn=game2">magnet2</a>]]></content:encoded>
        </item></channel></rss>"#;
        let channel = rss::Channel::read_from(xml.as_bytes()).unwrap();
        let item = &channel.items()[0];
        let magnet = magnet_from(item).unwrap();
        assert!(magnet
            .starts_with("magnet:?xt=urn:btih:CAB507494D02EBB1178B38F2E9D7BE299C86B862&dn=game"));
        assert_eq!(
            canonicalize_hash(extract_hash(&magnet).unwrap()).unwrap(),
            "cab507494d02ebb1178b38f2e9d7be299c86b862"
        );
    }

    #[test]
    fn content_only_feed_still_finds_magnet() {
        // Some WordPress feeds carry the magnet only in content:encoded.
        let xml = r#"<?xml version="1.0"?><rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/"><channel><item>
          <title>Game Repack v2.0</title>
          <link>https://site.example/game-repack-v2-0/</link>
          <description>no magnets here</description>
          <content:encoded><![CDATA[<p><a href="magnet:?xt=urn:btih:0A91FFE07210F925E020C1F316A33199B7E779FF&amp;dn=v2">magnet</a></p>]]></content:encoded>
        </item></channel></rss>"#;
        let channel = rss::Channel::read_from(xml.as_bytes()).unwrap();
        assert_eq!(
            extract_hash(&magnet_from(&channel.items()[0]).unwrap()).unwrap(),
            "0A91FFE07210F925E020C1F316A33199B7E779FF"
        );
    }

    #[test]
    fn nyaa_style_feed_maps() {
        let xml = r#"<?xml version="1.0"?><rss version="2.0" xmlns:nyaa="https://nyaa.si/xmlns/nyaa">
        <channel><item>
          <title>Show 01 [1080p]</title>
          <nyaa:infoHash>cab507494d02ebb1178b38f2e9d7be299c86b862</nyaa:infoHash>
          <nyaa:size>1.5 GiB</nyaa:size>
          <nyaa:seeders>42</nyaa:seeders>
          <nyaa:leechers>7</nyaa:leechers>
        </item></channel></rss>"#;
        let channel = rss::Channel::read_from(xml.as_bytes()).unwrap();
        let item = &channel.items()[0];
        assert_eq!(
            extension(item, "nyaa:infoHash").unwrap(),
            "cab507494d02ebb1178b38f2e9d7be299c86b862"
        );
        assert_eq!(extension(item, "nyaa:seeders").unwrap(), "42");
        assert_eq!(
            parse_size(&extension(item, "nyaa:size").unwrap()),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
        );
    }
}

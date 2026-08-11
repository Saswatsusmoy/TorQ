//! Source registry: the ten built-ins (declarative where possible) plus
//! TOML plugins dropped into `~/.config/torq/plugins/*.toml`.
//!
//! Plugins declare the same shapes the built-ins use (`kind = "json"` or
//! `kind = "rss"`), so a new site is config, not code.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use serde::Deserialize;

use crate::flat::{JsonDef, JsonSource};
use crate::rss_src::{RssDef, RssSource};
use crate::subsplease::SubsPlease;
use crate::types::{Source, SourceGroup};
use crate::x1337::{x1337_movies, x1337_tv};
use crate::yts::Yts;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum PluginDef {
    Json(Box<JsonDef>),
    Rss(Box<RssDef>),
}

pub struct Registry {
    pub sources: Vec<Arc<dyn Source>>,
}

impl Registry {
    pub fn builtin() -> Self {
        let flat = |def: JsonDef| JsonSource::new(def) as Arc<dyn Source>;
        let rss = |def: RssDef| RssSource::new(def) as Arc<dyn Source>;
        let mut sources: Vec<Arc<dyn Source>> = vec![
            // Games: FitGirl alone (the only category that can run code).
            rss(RssDef {
                id: "fitgirl".into(),
                label: "FitGirl".into(),
                groups: vec![SourceGroup::Games],
                homepage: "https://fitgirl-repacks.site".into(),
                reports_health: false,
                hosts: vec!["https://fitgirl-repacks.site".into()],
                path: "/feed/".into(),
                search_path: Some("/".into()),
                search_param: Some("s".into()),
                search_extra: vec![("feed".into(), "rss2".into())],
                hash_field: None,
                size_field: None,
                seeders_field: None,
                leechers_field: None,
            }),
            flat(JsonDef {
                id: "eztv".into(),
                label: "EZTV".into(),
                groups: vec![SourceGroup::Tv],
                homepage: "https://eztvx.to".into(),
                reports_health: true,
                hosts: vec!["https://eztvx.to".into()],
                path: "/api/get-torrents".into(),
                query_extra: vec![],
                query_param: None,
                min_query: 0,
                ignore_query: true,
                // EZTV's API has no search: queries return nothing; browsing
                // (empty query) returns the latest batch.
                browse_path: Some("/api/get-torrents".into()),
                browse_query: vec![("limit".into(), "100".into()), ("page".into(), "1".into())],
                items: Some("torrents".into()),
                map: crate::flat::FieldMap {
                    info_hash: "hash".into(),
                    name: "title".into(),
                    size: Some("size_bytes".into()),
                    seeders: Some("seeds".into()),
                    leechers: Some("peers".into()),
                    num_files: None,
                    magnet: Some("magnet_url".into()),
                    added: Some("date_released_unix".into()),
                    added_format: "unix".into(),
                    category: None,
                },
                categories: vec![],
            }),
            flat(JsonDef {
                id: "tpb-movies".into(),
                label: "TPB".into(),
                groups: vec![SourceGroup::Movies],
                homepage: "https://thepiratebay.org".into(),
                reports_health: true,
                hosts: vec!["https://apibay.org".into()],
                path: "/q.php".into(),
                query_extra: vec![],
                query_param: Some("q".into()),
                min_query: 0,
                ignore_query: false,
                browse_path: Some("/precompiled/data_top100_207.json".into()),
                browse_query: vec![],
                items: None,
                map: crate::flat::FieldMap {
                    info_hash: "info_hash".into(),
                    name: "name".into(),
                    size: Some("size".into()),
                    seeders: Some("seeders".into()),
                    leechers: Some("leechers".into()),
                    num_files: Some("num_files".into()),
                    magnet: None,
                    added: Some("added".into()),
                    added_format: "unix".into(),
                    category: Some("category".into()),
                },
                categories: vec![201, 202, 207, 209],
            }),
            flat(JsonDef {
                id: "tpb-tv".into(),
                label: "TPB".into(),
                groups: vec![SourceGroup::Tv],
                homepage: "https://thepiratebay.org".into(),
                reports_health: true,
                hosts: vec!["https://apibay.org".into()],
                path: "/q.php".into(),
                query_extra: vec![],
                query_param: Some("q".into()),
                min_query: 0,
                ignore_query: false,
                browse_path: Some("/precompiled/data_top100_208.json".into()),
                browse_query: vec![],
                items: None,
                map: crate::flat::FieldMap {
                    info_hash: "info_hash".into(),
                    name: "name".into(),
                    size: Some("size".into()),
                    seeders: Some("seeders".into()),
                    leechers: Some("leechers".into()),
                    num_files: Some("num_files".into()),
                    magnet: None,
                    added: Some("added".into()),
                    added_format: "unix".into(),
                    category: Some("category".into()),
                },
                categories: vec![205, 208],
            }),
            flat(JsonDef {
                id: "bittorrented".into(),
                label: "BitTorrented".into(),
                groups: vec![SourceGroup::Movies, SourceGroup::Tv],
                homepage: "https://bittorrented.com".into(),
                reports_health: true,
                hosts: vec!["https://bittorrented.com".into()],
                path: "/api/search/torrents".into(),
                query_extra: vec![
                    ("type".into(), "video".into()),
                    ("limit".into(), "50".into()),
                    ("sortBy".into(), "seeders".into()),
                    ("sortOrder".into(), "desc".into()),
                ],
                query_param: Some("q".into()),
                min_query: 3,
                ignore_query: false,
                browse_path: None,
                browse_query: vec![],
                items: Some("results".into()),
                map: crate::flat::FieldMap {
                    info_hash: "torrent_infohash".into(),
                    name: "torrent_name".into(),
                    size: Some("torrent_total_size".into()),
                    seeders: Some("torrent_seeders".into()),
                    leechers: Some("torrent_leechers".into()),
                    num_files: Some("torrent_file_count".into()),
                    magnet: None,
                    added: Some("torrent_created_at".into()),
                    added_format: "iso".into(),
                    category: None,
                },
                categories: vec![],
            }),
            rss(RssDef {
                id: "nyaa".into(),
                label: "Nyaa".into(),
                groups: vec![SourceGroup::Anime],
                homepage: "https://nyaa.si".into(),
                reports_health: true,
                hosts: vec!["https://nyaa.si".into()],
                path: "/".into(),
                search_path: None,
                search_param: Some("q".into()),
                search_extra: vec![
                    ("page".into(), "rss".into()),
                    ("c".into(), "0_0".into()),
                    ("f".into(), "0".into()),
                ],
                hash_field: Some("nyaa:infoHash".into()),
                size_field: Some("nyaa:size".into()),
                seeders_field: Some("nyaa:seeders".into()),
                leechers_field: Some("nyaa:leechers".into()),
            }),
            Arc::new(Yts) as Arc<dyn Source>,
            Arc::new(SubsPlease) as Arc<dyn Source>,
            x1337_movies(),
            x1337_tv(),
        ];
        sources.sort_by(|a, b| a.label().cmp(b.label()));
        Self { sources }
    }

    /// Load plugin TOML files from `dir` (skipped when absent/unreadable).
    pub fn plugins(dir: &Path) -> Vec<Arc<dyn Source>> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return vec![];
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            match toml::from_str::<PluginDef>(&raw).context(path.display().to_string()) {
                Ok(PluginDef::Json(def)) => out.push(JsonSource::new(*def) as Arc<dyn Source>),
                Ok(PluginDef::Rss(def)) => out.push(RssSource::new(*def) as Arc<dyn Source>),
                Err(e) => tracing::warn!("skipping plugin {}: {e:#}", path.display()),
            }
        }
        out
    }

    pub fn all() -> Self {
        let mut sources = Self::builtin().sources;
        sources.extend(Self::plugins(&crate::plugin_dir()));
        Self { sources }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_covers_all_sources() {
        let reg = Registry::builtin();
        let ids: Vec<&str> = reg.sources.iter().map(|s| s.id()).collect();
        for want in [
            "fitgirl",
            "eztv",
            "tpb-movies",
            "tpb-tv",
            "bittorrented",
            "nyaa",
            "yts",
            "subsplease",
            "x1337-movies",
            "x1337-tv",
        ] {
            assert!(ids.contains(&want), "missing {want}: {ids:?}");
        }
    }

    #[test]
    fn plugin_toml_parses() {
        let json_plugin: PluginDef = toml::from_str(
            r#"
            kind = "json"
            id = "mysite"
            label = "MySite"
            homepage = "https://mysite.example"
            hosts = ["https://mysite.example"]
            path = "/api"
            items = "rows"
            [map]
            info_hash = "hash"
            name = "name"
        "#,
        )
        .unwrap();
        assert!(matches!(json_plugin, PluginDef::Json(_)));

        let rss_plugin: PluginDef = toml::from_str(
            r#"
            kind = "rss"
            id = "myrss"
            label = "MyRSS"
            homepage = "https://x.example"
            hosts = ["https://x.example"]
            path = "/feed"
        "#,
        )
        .unwrap();
        assert!(matches!(rss_plugin, PluginDef::Rss(_)));
    }
}

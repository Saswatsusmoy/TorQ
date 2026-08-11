//! Declarative JSON-API source: one config drives EZTV, TPB (movies/TV), and
//! BitTorrented. Plugins reuse this same struct (deserialized from TOML), so
//! adding a site never requires code.

use std::sync::Arc;

use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;

use crate::types::{Source, SourceGroup, TorrentResult};
use crate::util::{build_magnet, fetch_with_failover};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldMap {
    pub info_hash: String,
    pub name: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub seeders: Option<String>,
    #[serde(default)]
    pub leechers: Option<String>,
    #[serde(default)]
    pub num_files: Option<String>,
    #[serde(default)]
    pub magnet: Option<String>,
    #[serde(default)]
    pub added: Option<String>,
    /// "unix" (number, default) or "iso" (RFC3339 string).
    #[serde(default)]
    pub added_format: String,
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonDef {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub groups: Vec<SourceGroup>,
    pub homepage: String,
    /// Defaults to true; set false for feeds without swarm data.
    #[serde(default = "yes")]
    pub reports_health: bool,
    /// Failover hosts; the first that answers wins.
    pub hosts: Vec<String>,
    pub path: String,
    /// Static query params appended to every request.
    #[serde(default)]
    pub query_extra: Vec<(String, String)>,
    /// Name of the query parameter carrying the search text.
    #[serde(default)]
    pub query_param: Option<String>,
    /// Sources that reject short queries (BitTorrented needs >= 3 chars).
    #[serde(default)]
    pub min_query: usize,
    /// EZTV: the API ignores queries and always returns the latest batch.
    #[serde(default)]
    pub ignore_query: bool,
    /// When the query is empty, request this path instead of `path` (TPB
    /// top-100; browse mode) with `browse_query` params.
    #[serde(default)]
    pub browse_path: Option<String>,
    #[serde(default)]
    pub browse_query: Vec<(String, String)>,
    /// Dotted path to the item array; None = response root is the array.
    #[serde(default)]
    pub items: Option<String>,
    pub map: FieldMap,
    /// TPB: keep only rows whose category is in this set.
    #[serde(default)]
    pub categories: Vec<i64>,
}

fn yes() -> bool {
    true
}

pub struct JsonSource {
    pub def: JsonDef,
}

impl JsonSource {
    pub fn new(def: JsonDef) -> Arc<Self> {
        Arc::new(Self { def })
    }
}

#[async_trait::async_trait]
impl Source for JsonSource {
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
        if !self.def.ignore_query && q.len() < self.def.min_query {
            return Ok(vec![]);
        }
        if self.def.ignore_query && !q.is_empty() {
            return Ok(vec![]);
        }
        let def = &self.def;
        let path = if q.is_empty() {
            match &def.browse_path {
                Some(p) => {
                    let params = build_query(&def.browse_query, None);
                    if params.is_empty() {
                        p.clone()
                    } else {
                        format!("{p}?{params}")
                    }
                }
                None => return Ok(vec![]),
            }
        } else {
            let params = build_query(&def.query_extra, def.query_param.as_deref().map(|p| (p, q)));
            if params.is_empty() {
                def.path.clone()
            } else {
                format!("{}?{params}", def.path)
            }
        };

        let body = fetch_with_failover(client, &def.hosts, &path).await?;
        let json: Value = serde_json::from_str(&body).context("parsing JSON")?;
        let items = match &def.items {
            Some(p) => get_path(&json, p)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            None => json.as_array().cloned().unwrap_or_default(),
        };

        let mut out = Vec::with_capacity(items.len());
        for it in items {
            if let Some(r) = self.map_row(&it, q) {
                out.push(r);
            }
        }
        Ok(out)
    }
}

impl JsonSource {
    fn map_row(&self, it: &Value, _query: &str) -> Option<TorrentResult> {
        let def = &self.def;
        let raw_hash = get_path(it, &def.map.info_hash)?.as_str()?;
        let info_hash = crate::util::canonicalize_hash(raw_hash)?;
        // TPB uses a zero hash for dead rows; drop those everywhere (harmless).
        if info_hash.chars().all(|c| c == '0') {
            return None;
        }
        if let (Some(cat_field), cats) = (&def.map.category, &def.categories) {
            let cat = get_path(it, cat_field).and_then(as_i64).unwrap_or(0);
            if !cats.is_empty() && !cats.contains(&cat) {
                return None;
            }
        }
        let name = get_path(it, &def.map.name)
            .and_then(as_str_owned)
            .unwrap_or_else(|| info_hash.clone());
        let magnet = match &def.map.magnet {
            Some(f) => get_path(it, f)
                .and_then(as_str_owned)
                .filter(|m| m.starts_with("magnet:")),
            None => None,
        }
        .unwrap_or_else(|| build_magnet(&info_hash, &name));
        let added = def
            .map
            .added
            .as_ref()
            .and_then(|f| get_path(it, f))
            .and_then(|v| match def.map.added_format.as_str() {
                "iso" => v.as_str().and_then(parse_rfc3339),
                _ => as_i64(v),
            });
        Some(TorrentResult {
            info_hash,
            name,
            size_bytes: def
                .map
                .size
                .as_ref()
                .and_then(|f| get_path(it, f))
                .and_then(as_u64)
                .unwrap_or(0),
            seeders: def
                .map
                .seeders
                .as_ref()
                .and_then(|f| get_path(it, f))
                .and_then(as_u64)
                .unwrap_or(0) as u32,
            leechers: def
                .map
                .leechers
                .as_ref()
                .and_then(|f| get_path(it, f))
                .and_then(as_u64)
                .unwrap_or(0) as u32,
            num_files: def
                .map
                .num_files
                .as_ref()
                .and_then(|f| get_path(it, f))
                .and_then(as_u64)
                .map(|v| v as u32),
            source: def.id.clone(),
            magnet,
            added,
        })
    }
}

fn build_query(extra: &[(String, String)], search: Option<(&str, &str)>) -> String {
    let mut params: Vec<(String, String)> = extra.to_vec();
    if let Some((k, v)) = search {
        params.push((k.to_string(), v.to_string()));
    }
    if params.is_empty() {
        return String::new();
    }
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode(s: &str) -> String {
    urlencoding(s)
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

/// Dotted-path accessor: `get_path(&json, "data.movies")`.
fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for part in path.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur)
}

fn as_str_owned(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// Value may be a number or a numeric string ("875").
fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Best-effort RFC3339 parse (BitTorrented timestamps); seconds precision.
fn parse_rfc3339(s: &str) -> Option<i64> {
    let t = s.strip_suffix('Z').or_else(|| s.strip_suffix("+00:00"))?;
    let (date, time) = t.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let mut t = time.split(':');
    let (h, mi, se): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.get(..2)?.parse().ok()?,
    );
    // Days-from-civil (Howard Hinnant), valid for the modern era.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> JsonDef {
        serde_json::from_value(serde_json::json!({
            "id": "t", "label": "T", "homepage": "https://t", "hosts": ["https://t"],
            "path": "/api", "items": "results",
            "map": { "info_hash": "h", "name": "n", "size": "s", "seeders": "se",
                     "leechers": "le", "added": "a" }
        }))
        .unwrap()
    }

    #[test]
    fn maps_rows_and_drops_bad_hashes() {
        let src = JsonSource::new(def());
        let body = serde_json::json!({"results": [
            {"h": "cab507494d02ebb1178b38f2e9d7be299c86b862", "n": "A", "s": 100, "se": "12", "le": 3, "a": 1700000000},
            {"h": "0000000000000000000000000000000000000000", "n": "dead"},
            {"h": "short", "n": "bad"}
        ]});
        let rows: Vec<TorrentResult> = body["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|it| src.map_row(it, ""))
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "A");
        assert_eq!(rows[0].seeders, 12);
        assert_eq!(rows[0].added, Some(1700000000));
        assert!(rows[0].magnet.starts_with("magnet:?xt=urn:btih:cab507"));
    }

    #[test]
    fn category_filter_applies() {
        let mut d = def();
        d.map.category = Some("cat".into());
        d.categories = vec![201];
        let src = JsonSource::new(d);
        let row = |cat: i64| serde_json::json!({"h": "cab507494d02ebb1178b38f2e9d7be299c86b862", "n": "A", "cat": cat});
        assert!(src.map_row(&row(201), "").is_some());
        assert!(src.map_row(&row(205), "").is_none());
    }

    #[test]
    fn rfc3339_and_sizes() {
        assert_eq!(parse_rfc3339("2026-07-17T10:00:00Z"), Some(1784282400));
        assert_eq!(crate::util::parse_size("1.5 GB"), 1610612736);
    }
}

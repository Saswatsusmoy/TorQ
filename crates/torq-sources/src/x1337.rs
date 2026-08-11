//! 1337x: the one HTML-scraped source (movies + TV). Multi-host failover,
//! result rows from the search table, magnets fetched from detail pages
//! (bounded: only the top few rows, mirroring torlink).

use std::sync::Arc;

use scraper::{Html, Selector};

use crate::types::{Source, SourceGroup, TorrentResult};
use crate::util::{fetch_with_failover, parse_size};

const HOSTS: &[&str] = &[
    "https://1337x.to",
    "https://1337x.st",
    "https://x1337x.ws",
    "https://1337xx.to",
];
const MAX_DETAILS: usize = 4;

struct Row {
    name: String,
    path: String,
    seeders: u32,
    leechers: u32,
    size_bytes: u64,
}

pub struct X1337 {
    cat: &'static str,
    id: &'static str,
    group: SourceGroup,
}

pub fn x1337_movies() -> Arc<X1337> {
    Arc::new(X1337 {
        cat: "Movies",
        id: "x1337-movies",
        group: SourceGroup::Movies,
    })
}

pub fn x1337_tv() -> Arc<X1337> {
    Arc::new(X1337 {
        cat: "TV",
        id: "x1337-tv",
        group: SourceGroup::Tv,
    })
}

#[async_trait::async_trait]
impl Source for X1337 {
    fn id(&self) -> &str {
        self.id
    }
    fn label(&self) -> &str {
        "1337x"
    }
    fn groups(&self) -> &[SourceGroup] {
        std::slice::from_ref(&self.group)
    }
    fn homepage(&self) -> &str {
        "https://1337x.to"
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
            format!("/popular-{}", self.cat.to_lowercase())
        } else {
            format!("/category-search/{}/{}/1/", q.replace(' ', "+"), self.cat)
        };
        let hosts: Vec<String> = HOSTS.iter().map(|s| s.to_string()).collect();
        let html = fetch_with_failover(client, &hosts, &path).await?;
        let rows = parse_rows(&html);

        // Detail pages carry the magnets; fetch the top rows in parallel.
        // A row whose detail page fails or lacks a magnet is dropped — a
        // magnet-less result is undownloadable (torlink behavior).
        let mut out = Vec::with_capacity(rows.len());
        for row in rows.into_iter().take(MAX_DETAILS) {
            let Ok(detail) = fetch_with_failover(client, &hosts, &row.path).await else {
                continue;
            };
            let Some(magnet) = parse_magnet(&detail) else {
                continue;
            };
            let info_hash = magnet
                .split("urn:btih:")
                .nth(1)
                .and_then(|s| s.split('&').next())
                .map(str::to_lowercase)
                .unwrap_or_default();
            if info_hash.len() != 40 {
                continue;
            }
            out.push(TorrentResult {
                info_hash,
                name: row.name,
                size_bytes: row.size_bytes,
                seeders: row.seeders,
                leechers: row.leechers,
                num_files: None,
                source: self.id.to_string(),
                magnet,
                added: None,
            });
        }
        Ok(out)
    }
}

fn parse_rows(html: &str) -> Vec<Row> {
    let doc = Html::parse_document(html);
    let tr = Selector::parse("tr").expect("valid selector");
    let link = Selector::parse("a[href^='/torrent/']").expect("valid selector");
    let seeds = Selector::parse(".coll-2.seeds").expect("valid selector");
    let leeches = Selector::parse(".coll-3.leeches").expect("valid selector");
    let size = Selector::parse(".coll-4.size").expect("valid selector");

    doc.select(&tr)
        .filter_map(|row| {
            let a = row.select(&link).next()?;
            let seeders = row
                .select(&seeds)
                .next()
                .and_then(|e| e.text().next())
                .and_then(|t| t.trim().parse().ok())
                .unwrap_or(0);
            let leechers = row
                .select(&leeches)
                .next()
                .and_then(|e| e.text().next())
                .and_then(|t| t.trim().parse().ok())
                .unwrap_or(0);
            let size_bytes = row
                .select(&size)
                .next()
                .and_then(|e| e.text().next())
                .map(parse_size)
                .unwrap_or(0);
            Some(Row {
                name: a.text().collect::<String>().trim().to_string(),
                path: a.value().attr("href")?.to_string(),
                seeders,
                leechers,
                size_bytes,
            })
        })
        .collect()
}

fn parse_magnet(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let a = Selector::parse("a[href^='magnet:?xt=urn:btih:']").ok()?;
    doc.select(&a)
        .next()
        .map(|e| e.value().attr("href").unwrap_or_default().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<html><body><table class="table-list">
        <tr><td class="coll-1 name"><a href="/torrent/123-A">A Movie</a></td>
            <td class="coll-2 seeds">45</td><td class="coll-3 leeches">5</td>
            <td class="coll-4 size">1.2 GiB</td></tr>
        <tr><td class="coll-1 name"><a href="/torrent/456-B">B Movie</a></td>
            <td class="coll-2 seeds">3</td><td class="coll-3 leeches">1</td>
            <td class="coll-4 size">800 MB</td></tr>
    </table></body></html>"#;

    #[test]
    fn parses_result_rows() {
        let rows = parse_rows(SAMPLE);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "A Movie");
        assert_eq!(rows[0].path, "/torrent/123-A");
        assert_eq!(rows[0].seeders, 45);
        assert_eq!(rows[0].size_bytes, (1.2 * 1024.0 * 1024.0 * 1024.0) as u64);
    }

    #[test]
    fn extracts_magnet_from_detail() {
        let html = r#"<a href="magnet:?xt=urn:btih:abc&amp;dn=x">download</a>"#;
        let m = parse_magnet(html).unwrap();
        assert!(m.starts_with("magnet:?xt=urn:btih:abc"));
    }
}

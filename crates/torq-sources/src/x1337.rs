//! 1337x: the one HTML-scraped source (movies + TV). Multi-host failover,
//! result rows from the search table, magnets fetched from detail pages
//! (bounded: only the top few rows, mirroring torlink).

use std::sync::Arc;

use scraper::{Html, Selector};

use crate::types::{Source, SourceGroup, TorrentResult};
use crate::util::{fetch_with_failover, parse_size};

const MAX_DETAILS: usize = 4;

/// A parsed row from the 1337x search table.
struct Row {
    name: String,
    path: String,
    seeders: u32,
    leechers: u32,
    size_bytes: u64,
}

pub struct X1337 {
    hosts: Vec<String>,
    cat: &'static str,
    id: &'static str,
    group: SourceGroup,
}

pub fn x1337_movies() -> Arc<X1337> {
    x1337_with_hosts(
        default_hosts(),
        "Movies",
        "x1337-movies",
        SourceGroup::Movies,
    )
}

pub fn x1337_tv() -> Arc<X1337> {
    x1337_with_hosts(default_hosts(), "TV", "x1337-tv", SourceGroup::Tv)
}

/// 1337x source against explicit mirrors (custom hosts, tests, or a local
/// fixture server for profiling).
pub fn x1337_with_hosts(
    hosts: Vec<String>,
    cat: &'static str,
    id: &'static str,
    group: SourceGroup,
) -> Arc<X1337> {
    Arc::new(X1337 {
        hosts,
        cat,
        id,
        group,
    })
}

fn default_hosts() -> Vec<String> {
    [
        "https://1337x.to",
        "https://1337x.st",
        "https://x1337x.ws",
        "https://1337xx.to",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
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
        let html = fetch_with_failover(client, &self.hosts, &path).await?;
        let rows = parse_rows(&html);
        Ok(self
            .fetch_all_details(client, rows.into_iter().take(MAX_DETAILS).collect())
            .await)
    }
}

impl X1337 {
    /// Fetch the magnet-bearing detail pages for `rows` in parallel. These
    /// round trips dominate search wall time and are independent; a row whose
    /// page fails or lacks a usable magnet is dropped — a magnet-less result
    /// is undownloadable (torlink behavior). Order is preserved (top rows
    /// first), matching the previous sequential version.
    async fn fetch_all_details(
        &self,
        client: &reqwest::Client,
        rows: Vec<Row>,
    ) -> Vec<TorrentResult> {
        let futs = rows
            .into_iter()
            .map(|row| self.fetch_details(client, &self.hosts, row));
        futures::future::join_all(futs)
            .await
            .into_iter()
            .flatten()
            .collect()
    }

    /// Fetch one 1337x detail page and map it to a result. `None` when the
    /// page fails, carries no magnet, or has an unusable infohash.
    async fn fetch_details(
        &self,
        client: &reqwest::Client,
        hosts: &[String],
        row: Row,
    ) -> Option<TorrentResult> {
        let detail = fetch_with_failover(client, hosts, &row.path).await.ok()?;
        let magnet = parse_magnet(&detail)?;
        let info_hash = magnet
            .split("urn:btih:")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .map(str::to_lowercase)
            .unwrap_or_default();
        if info_hash.len() != 40 {
            return None;
        }
        Some(TorrentResult {
            info_hash,
            name: row.name,
            size_bytes: row.size_bytes,
            seeders: row.seeders,
            leechers: row.leechers,
            num_files: None,
            source: self.id.to_string(),
            magnet,
            added: None,
        })
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
    use tokio::io::AsyncReadExt;

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

    /// The detail pages are fetched concurrently; this exercises the real
    /// join_all path against a local server and asserts order + mapping.
    #[tokio::test]
    async fn detail_fetch_batch_preserves_order_and_maps() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..4 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                // "/torrent/2" -> hash 3; respond by requested path so the
                // result is deterministic regardless of connection order.
                let idx: u64 = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|p| p.strip_prefix("/torrent/"))
                    .and_then(|p| p.trim_end_matches(['/', '?']).parse().ok())
                    .unwrap_or(0);
                let hash = format!("{:040x}", idx + 1);
                let body = format!(r#"<a href="magnet:?xt=urn:btih:{hash}&dn=x">m</a>"#);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });

        let client = reqwest::Client::new();
        let hosts = vec![format!("http://{addr}")];
        let src = x1337_with_hosts(hosts, "Movies", "x1337-movies", SourceGroup::Movies);
        let rows: Vec<Row> = (0..4)
            .map(|i| Row {
                name: format!("Movie {i}"),
                path: format!("/torrent/{i}"),
                seeders: 10 - i as u32,
                leechers: i as u32,
                size_bytes: 100 + i as u64,
            })
            .collect();
        let out = src.fetch_all_details(&client, rows).await;
        assert_eq!(out.len(), 4);
        for (i, r) in out.iter().enumerate() {
            assert_eq!(r.name, format!("Movie {i}"));
            assert_eq!(r.info_hash, format!("{:040x}", i + 1));
            assert_eq!(r.seeders, 10 - i as u32);
            assert_eq!(r.source, "x1337-movies");
            assert!(r.magnet.starts_with("magnet:?xt=urn:btih:"));
        }
    }

    /// A page that fails (connection refused) or lacks a magnet drops the
    /// row instead of failing the whole batch.
    #[tokio::test]
    async fn detail_fetch_batch_drops_bad_rows() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // One connection is refused: the server only ever accepts once.
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            // No magnet on this page.
            let body = "<html><body>no download link</body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            use tokio::io::AsyncWriteExt;
            let _ = sock.write_all(resp.as_bytes()).await;
        });

        let client = reqwest::Client::new();
        let hosts = vec![format!("http://{addr}")];
        let src = x1337_with_hosts(hosts, "Movies", "x1337-movies", SourceGroup::Movies);
        let row = |n: usize| Row {
            name: format!("Movie {n}"),
            path: format!("/torrent/{n}"),
            seeders: 1,
            leechers: 0,
            size_bytes: 1,
        };
        // Two rows: one page exists but has no magnet, one connection fails.
        let out = src.fetch_all_details(&client, vec![row(0), row(1)]).await;
        assert!(out.is_empty());
    }
}

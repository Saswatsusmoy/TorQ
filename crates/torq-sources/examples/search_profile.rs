//! Search-pipeline profiler for torq.
//!
//! Measures the real production search code with realistic payloads, not toy
//! data. Three modes:
//!
//! ```text
//! cargo run --release --example search_profile -- replay [--iters N]
//! cargo run --release --example search_profile -- live [--iters N]
//! cargo run --release --example search_profile -- replay --dhat
//! ```
//!
//! - `replay`: deterministic. All eight fixture-backed sources run their real
//!   `search()` — real fetch code, real parse code — against a local fixture
//!   server serving captured real payloads from the live sites (see
//!   `examples/fixtures/README.md`; the 1337x pages are synthesized because
//!   the site Cloudflare-blocks this network). yts and subsplease hardcode
//!   their hosts in code, so they are only profiled in `live` mode.
//! - `live`: real network, all ten builtin sources, the production path —
//!   including failover latency.
//! - `--dhat`: heap-allocation profile of one replay run (dhat global
//!   allocator; report printed at exit).
//!
//! Each mode times: per-source fetch+parse+map (sequential), the full
//! concurrent aggregate (`search_all`), the pure dedupe+sort stage (rows fed
//! back through instant sources), and the API JSON serialization.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use torq_sources::Registry;
use torq_sources::aggregate::search_all;
use torq_sources::flat::{FieldMap, JsonDef, JsonSource};
use torq_sources::rss_src::{RssDef, RssSource};
use torq_sources::types::{Source, SourceGroup, TorrentResult, http_client};
use torq_sources::x1337::x1337_with_hosts;

// dhat's allocator is a no-op (one atomic check) until Profiler::new_heap()
// runs; --dhat (requires the `dhat` feature) starts it, every other mode
// pays one branch per allocation. Without the feature the allocator is the
// system one — clean for CPU sampling.
#[cfg(feature = "dhat")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/fixtures");

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

// Fixture server: a bare-bones HTTP/1.1 responder that serves the captured
// payloads by exact request path (path + query, as fetch_with_failover
// builds it). One server per source so path collisions (e.g. both fitgirl
// and nyaa browse on "/") stay isolated.

struct FixtureServer {
    base: String,
}

async fn spawn_fixture_server(fixtures: Vec<(String, Vec<u8>)>) -> FixtureServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture server");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let map = Arc::new(fixtures.into_iter().collect::<HashMap<_, _>>());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                continue;
            };
            let map = Arc::clone(&map);
            tokio::spawn(async move {
                // Keep-alive, like real sites: reuse the connection so the
                // profile matches production reqwest behavior.
                let mut buf = Vec::with_capacity(4096);
                loop {
                    let mut tmp = [0u8; 1024];
                    let Ok(n) = sock.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        if buf.len() > 65536 {
                            return;
                        }
                        continue;
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]);
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    let body = map.get(path).cloned().unwrap_or_default();
                    let mut resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    resp.extend_from_slice(&body);
                    if sock.write_all(&resp).await.is_err() {
                        return;
                    }
                    buf.drain(..head_end + 4);
                }
            });
        }
    });
    FixtureServer { base }
}

// Replay source construction. These defs mirror registry.rs exactly, with
// `hosts` pointed at the local fixture server.

fn fitgirl_def(base: &str) -> RssDef {
    RssDef {
        id: "fitgirl".into(),
        label: "FitGirl".into(),
        groups: vec![SourceGroup::Games],
        homepage: "https://fitgirl-repacks.site".into(),
        reports_health: false,
        hosts: vec![base.into()],
        path: "/".into(),
        search_path: Some("/".into()),
        search_param: Some("s".into()),
        search_extra: vec![("feed".into(), "rss2".into())],
        ..Default::default()
    }
}

fn eztv_def(base: &str) -> JsonDef {
    JsonDef {
        id: "eztv".into(),
        label: "EZTV".into(),
        groups: vec![SourceGroup::Tv],
        homepage: "https://eztvx.to".into(),
        reports_health: true,
        hosts: vec![base.into()],
        path: "/api/get-torrents".into(),
        query_extra: vec![],
        query_param: None,
        min_query: 0,
        ignore_query: true,
        browse_path: Some("/api/get-torrents".into()),
        browse_query: vec![("limit".into(), "100".into()), ("page".into(), "1".into())],
        items: Some("torrents".into()),
        map: FieldMap {
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
    }
}

fn tpb_def(base: &str, id: &str, browse_file: &str) -> JsonDef {
    JsonDef {
        id: id.into(),
        label: "TPB".into(),
        groups: vec![SourceGroup::Movies],
        homepage: "https://thepiratebay.org".into(),
        reports_health: true,
        hosts: vec![base.into()],
        path: "/q.php".into(),
        query_extra: vec![],
        query_param: Some("q".into()),
        min_query: 0,
        ignore_query: false,
        browse_path: Some(format!("/precompiled/{browse_file}")),
        browse_query: vec![],
        items: None,
        map: FieldMap {
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
        categories: if id.ends_with("movies") {
            vec![201, 202, 207, 209]
        } else {
            vec![205, 208]
        },
    }
}

fn bittorrented_def(base: &str) -> JsonDef {
    JsonDef {
        id: "bittorrented".into(),
        label: "BitTorrented".into(),
        groups: vec![SourceGroup::Movies, SourceGroup::Tv],
        homepage: "https://bittorrented.com".into(),
        reports_health: true,
        hosts: vec![base.into()],
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
        map: FieldMap {
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
    }
}

fn nyaa_def(base: &str) -> RssDef {
    RssDef {
        id: "nyaa".into(),
        label: "Nyaa".into(),
        groups: vec![SourceGroup::Anime],
        homepage: "https://nyaa.si".into(),
        reports_health: true,
        hosts: vec![base.into()],
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
    }
}

/// (source id, fixtures keyed by the exact request path the source builds,
/// and the on-disk fixture file). `search` vs `browse` tables match what the
/// sources request for a query vs an empty query.
struct FixtureTable {
    query: &'static str,
    entries: &'static [(&'static str, &'static str, &'static str)],
}

const SEARCH_TABLE: FixtureTable = FixtureTable {
    query: "inception",
    entries: &[
        // id, request path, fixture file
        ("fitgirl", "/?feed=rss2&s=inception", "fitgirl_search.xml"),
        (
            "eztv",
            "/api/get-torrents?limit=100&page=1",
            "eztv_browse.json",
        ),
        ("tpb-movies", "/q.php?q=inception", "tpb_search.json"),
        ("tpb-tv", "/q.php?q=inception", "tpb_search.json"),
        (
            "bittorrented",
            "/api/search/torrents?type=video&limit=50&sortBy=seeders&sortOrder=desc&q=inception",
            "bittorrented_search.json",
        ),
        (
            "nyaa",
            "/?page=rss&c=0_0&f=0&q=inception",
            "nyaa_inception.xml",
        ),
        (
            "x1337-movies",
            "/category-search/inception/Movies/1/",
            "x1337_list.html",
        ),
        (
            "x1337-tv",
            "/category-search/inception/TV/1/",
            "x1337_list.html",
        ),
    ],
};

const BROWSE_TABLE: FixtureTable = FixtureTable {
    query: "",
    entries: &[
        ("fitgirl", "/", "fitgirl_feed.xml"),
        (
            "eztv",
            "/api/get-torrents?limit=100&page=1",
            "eztv_browse.json",
        ),
        (
            "tpb-movies",
            "/precompiled/data_top100_207.json",
            "tpb_browse.json",
        ),
        (
            "tpb-tv",
            "/precompiled/data_top100_208.json",
            "tpb_tv_browse.json",
        ),
        ("nyaa", "/", "nyaa_browse.xml"),
    ],
};

async fn replay_sources(table: &FixtureTable) -> Vec<Arc<dyn Source>> {
    let mut out: Vec<Arc<dyn Source>> = Vec::new();
    for &(id, key, file) in table.entries {
        let src: Arc<dyn Source> = if id.starts_with("x1337") {
            // The detail pages carry the magnets; serve the synthesized list
            // plus the top MAX_DETAILS=4 detail pages at the exact paths the
            // parsed rows request (/torrent/{i}-inception-{i} in the fixture).
            let mut fixtures = vec![(key.to_string(), fixture(file))];
            for i in 0..4 {
                fixtures.push((
                    format!("/torrent/{i}-inception-{i}"),
                    fixture(&format!("x1337_detail_{i}.html")),
                ));
            }
            let server = spawn_fixture_server(fixtures).await;
            let cat = if id == "x1337-movies" { "Movies" } else { "TV" };
            let group = if id == "x1337-movies" {
                SourceGroup::Movies
            } else {
                SourceGroup::Tv
            };
            x1337_with_hosts(vec![server.base], cat, id, group)
        } else {
            let server = spawn_fixture_server(vec![(key.to_string(), fixture(file))]).await;
            let base = server.base.clone();
            match id {
                "fitgirl" => RssSource::new(fitgirl_def(&base)),
                "eztv" => JsonSource::new(eztv_def(&base)),
                "tpb-movies" | "tpb-tv" => {
                    JsonSource::new(tpb_def(&base, id, "data_top100_207.json"))
                }
                "bittorrented" => JsonSource::new(bittorrented_def(&base)),
                "nyaa" => RssSource::new(nyaa_def(&base)),
                _ => unreachable!(),
            }
        };
        out.push(src);
    }
    out
}

// Instant source: returns pre-mapped rows with no fetch — isolates the
// aggregate dedupe+sort stage.

struct InstantSource {
    id: String,
    rows: Vec<TorrentResult>,
}

#[async_trait::async_trait]
impl Source for InstantSource {
    fn id(&self) -> &str {
        &self.id
    }
    fn label(&self) -> &str {
        &self.id
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
    async fn search(&self, _q: &str, _c: &reqwest::Client) -> anyhow::Result<Vec<TorrentResult>> {
        Ok(self.rows.clone())
    }
}

// Timing + stats

fn stats(samples: &[Duration]) -> (f64, f64, f64, f64, f64) {
    // (mean, median, p95, min, max) in milliseconds
    let mut v: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e3).collect();
    v.sort_by(f64::total_cmp);
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let median = v[v.len() / 2];
    let p95 = v[(v.len() as f64 * 0.95) as usize];
    (mean, median, p95, v[0], v[v.len() - 1])
}

fn print_stats(name: &str, samples: &[Duration]) {
    let (mean, median, p95, min, max) = stats(samples);
    println!(
        "{name:<38} {mean:>9.3} {median:>9.3} {p95:>9.3} {min:>9.3} {max:>9.3}  ({:>5} iters)",
        samples.len()
    );
}

async fn time_async<F, T>(f: F) -> (Duration, T)
where
    F: std::future::Future<Output = T>,
{
    let t = Instant::now();
    let v = f.await;
    (t.elapsed(), v)
}

// Replay mode

async fn run_replay(iters: usize, dhat: bool) -> Result<()> {
    #[cfg(feature = "dhat")]
    let _profiler = dhat.then(dhat::Profiler::new_heap);
    #[cfg(not(feature = "dhat"))]
    if dhat {
        anyhow::bail!("--dhat requires building with --features dhat");
    }
    let client = http_client(None).context("http client")?;
    let sources = replay_sources(&SEARCH_TABLE).await;

    println!("== replay: search query {:?}", SEARCH_TABLE.query);
    println!(
        "   fixtures: real payloads captured from live sites (see examples/fixtures/README.md)"
    );
    println!("   yts/subsplease: hardcoded hosts, live mode only");

    // 1. per-source sequential fetch+parse+map
    println!("\nper-source fetch+parse+map (sequential):");
    println!(
        "{:<38} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "source", "mean ms", "median", "p95", "min", "max"
    );
    let mut per_source = Vec::new();
    let mut total_rows = 0usize;
    for s in &sources {
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (d, rows) = time_async(s.search(SEARCH_TABLE.query, &client)).await;
            samples.push(d);
            if samples.len() == 1 {
                let n = rows.map(|r| r.len()).unwrap_or(0);
                total_rows += n;
                per_source.push((s.id().to_string(), n));
            }
        }
        print_stats(
            &format!("{} ({} rows)", s.id(), per_source.last().unwrap().1),
            &samples,
        );
    }

    // 2. full concurrent aggregate
    let mut agg_samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (d, report) = time_async(search_all(&sources, &client, SEARCH_TABLE.query, None)).await;
        agg_samples.push(d);
        if agg_samples.len() == 1 {
            let deduped = report.results.len();
            println!(
                "\nfull concurrent search_all: {deduped}/{} rows after dedupe ({} sources)",
                total_rows,
                sources.len()
            );
        }
    }
    println!("\nfull concurrent search_all (fetch+parse+map+dedupe+sort):");
    print_stats("search_all", &agg_samples);

    // 3. pure aggregate: pre-mapped rows fed back through instant sources
    let mut inst: Vec<Arc<dyn Source>> = Vec::new();
    for s in &sources {
        let rows = s.search(SEARCH_TABLE.query, &client).await?;
        inst.push(Arc::new(InstantSource {
            id: s.id().to_string(),
            rows,
        }));
    }
    let mut pure_samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (d, _) = time_async(search_all(&inst, &client, SEARCH_TABLE.query, None)).await;
        pure_samples.push(d);
    }
    println!("\npure aggregate stage (dedupe+sort, no fetch/parse):");
    print_stats("dedupe+sort", &pure_samples);

    // 5. API JSON serialization of the report
    let report = search_all(&sources, &client, SEARCH_TABLE.query, None).await;
    let mut ser_samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        let _json = serde_json::to_string(&report)?;
        ser_samples.push(t.elapsed());
    }
    println!(
        "\nAPI JSON serialization ({} results):",
        report.results.len()
    );
    print_stats("serde_json::to_string(report)", &ser_samples);

    // 6. browse mode (empty query)
    let bsources = replay_sources(&BROWSE_TABLE).await;
    let mut browse_samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (d, report) = time_async(search_all(&bsources, &client, "", None)).await;
        browse_samples.push(d);
        if browse_samples.len() == 1 {
            println!(
                "\nbrowse (empty query): {} rows after dedupe ({} sources)",
                report.results.len(),
                bsources.len()
            );
        }
    }
    println!("full concurrent browse search_all:");
    print_stats("search_all (browse)", &browse_samples);

    Ok(())
}

// Live mode

async fn run_live(iters: usize) -> Result<()> {
    let client = http_client(None).context("http client")?;
    let sources = Registry::builtin().sources;
    let query = "inception";
    println!(
        "== live: all {} builtin sources, real network, query {query:?}",
        sources.len()
    );
    println!("   includes failover latency and the sequential 1337x detail chain");

    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (d, report) = time_async(search_all(&sources, &client, query, None)).await;
        samples.push(d);
        if samples.len() == 1 {
            println!(
                "first run: {} rows, offline: {:?}",
                report.results.len(),
                report.offline
            );
        }
    }
    println!("\nfull concurrent search_all (wall clock, includes network):");
    print_stats("search_all (live)", &samples);

    println!("\nper-source (sequential, includes failover):");
    println!(
        "{:<38} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "source", "mean ms", "median", "p95", "min", "max"
    );
    for s in &sources {
        let mut ss = Vec::new();
        for _ in 0..iters {
            let (d, rows) = time_async(s.search(query, &client)).await;
            ss.push(d);
            let _ = rows;
        }
        print_stats(&format!("{} ({iters} iters)", s.id()), &ss);
    }
    Ok(())
}

// Sample mode: run search_all forever so an external sampler (sample,
// xctrace, samply) can attach and attribute CPU time across the pipeline.

async fn run_sample() -> Result<()> {
    let client = http_client(None).context("http client")?;
    let sources = replay_sources(&SEARCH_TABLE).await;
    eprintln!(
        "sampling: search_all loop started (pid {})",
        std::process::id()
    );
    let mut n = 0u64;
    loop {
        let report = search_all(&sources, &client, SEARCH_TABLE.query, None).await;
        n += 1;
        if n.is_multiple_of(1000) {
            eprintln!("sampling: {n} iterations, {} results", report.results.len());
        }
    }
}

fn usage() {
    println!(
        "usage: search_profile <replay|live|sample> [--iters N] [--dhat]\n  \
         replay  deterministic fixture replay (real captured payloads)\n  \
         live    real network, all builtin sources\n  \
         sample  run search_all forever (attach a CPU sampler)"
    );
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut mode = None;
    let mut iters = 25usize;
    let mut dhat = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "replay" | "live" | "sample" => mode = Some(args[i].clone()),
            "--iters" => {
                i += 1;
                iters = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .context("--iters needs a number")?;
            }
            "--dhat" => dhat = true,
            other => anyhow::bail!("unknown arg {other:?}"),
        }
        i += 1;
    }
    let Some(mode) = mode else {
        usage();
        return Ok(());
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    rt.block_on(async move {
        match mode.as_str() {
            "replay" => run_replay(iters, dhat).await,
            "live" => run_live(iters).await,
            "sample" => run_sample().await,
            _ => unreachable!(),
        }
    })
}

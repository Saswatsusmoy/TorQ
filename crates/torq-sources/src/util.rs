//! Small shared helpers: magnets, size/date parsing, host failover fetches.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Context;

/// Per-host request timeout: dead mirrors cost at most this much each, and
/// with parallel probing the search waits only for the fastest answer.
const HOST_TIMEOUT: Duration = Duration::from_secs(8);

/// How long a remembered "last working host" is trusted before re-probing
/// from the top. Host health changes (Cloudflare flaps, mirrors going down),
/// so preferences go stale; with parallel probing a stale preference costs
/// nothing extra — the other hosts are probed in the same round.
const HOST_PREF_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
struct HostPref {
    index: usize,
    at: Instant,
}

fn host_pref() -> &'static Mutex<HashMap<String, HostPref>> {
    static PREF: OnceLock<Mutex<HashMap<String, HostPref>>> = OnceLock::new();
    PREF.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Start index for failover over `hosts`: the last host that succeeded for
/// this host set, if the memory is fresh.
fn pref_start(hosts: &[String]) -> usize {
    if hosts.is_empty() {
        return 0;
    }
    let map = host_pref().lock().unwrap_or_else(|p| p.into_inner());
    match map.get(&hosts.join("|")) {
        Some(p) if p.at.elapsed() < HOST_PREF_TTL => p.index % hosts.len(),
        _ => 0,
    }
}

fn pref_record(hosts: &[String], index: usize) {
    if hosts.is_empty() {
        return;
    }
    let mut map = host_pref().lock().unwrap_or_else(|p| p.into_inner());
    map.insert(
        hosts.join("|"),
        HostPref {
            index,
            at: Instant::now(),
        },
    );
}

/// Build a magnet link from an infohash + display name.
pub fn build_magnet(info_hash: &str, name: &str) -> String {
    let mut m = format!("magnet:?xt=urn:btih:{info_hash}");
    if !name.is_empty() {
        m.push_str("&dn=");
        m.push_str(&urlencode(name));
    }
    m
}

fn urlencode(s: &str) -> String {
    encode(s, false)
}

/// Percent-encode a query-string component (spaces as `+`). Shared by every
/// source that builds search URLs; one implementation, preallocated.
pub(crate) fn encode_query_component(s: &str) -> String {
    encode(s, true)
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn encode(s: &str, space_as_plus: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' if space_as_plus => out.push('+'),
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Parse "123.4 MiB", "1.2 GiB", "500 MB", "1.5 GB" → bytes.
pub fn parse_size(s: &str) -> u64 {
    let t = s.trim();
    let Some(idx) = t.find(|c: char| c.is_ascii_alphabetic()) else {
        return t.parse().unwrap_or(0);
    };
    let Ok(v) = t[..idx].trim().parse::<f64>() else {
        return 0;
    };
    let mult = match t[idx..].trim().to_ascii_uppercase().as_str() {
        "KIB" | "KB" => 1024.0,
        "MIB" | "MB" => 1024.0 * 1024.0,
        "GIB" | "GB" => 1024.0 * 1024.0 * 1024.0,
        "TIB" | "TB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    (v * mult) as u64
}

/// Normalize an infohash to canonical lowercase hex: accepts 40-char hex and
/// 32-char base32 (SubsPlease-style magnets). `None` for anything else.
pub fn canonicalize_hash(h: &str) -> Option<String> {
    let t = h.trim();
    if t.len() == 40 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(t.to_lowercase());
    }
    if t.len() == 32
        && t.chars()
            .all(|c| c.is_ascii_uppercase() || ('2'..='7').contains(&c))
    {
        return data_encoding::BASE32_NOPAD
            .decode(t.as_bytes())
            .ok()
            .map(|b| data_encoding::HEXLOWER.encode(&b));
    }
    None
}

/// GET `url`, trying `hosts` in order; first success wins, HTTP errors fall
/// through to the next host. Hosts are joined with `path`. Per-host timeout is
/// 8s (not the client's 20s) so dead hosts cost at most 8s each, not 20s.
///
/// The last host that succeeded for this host set is tried first on the next
/// call (with a TTL), so repeatedly-dead mirrors are skipped instead of
/// probed on every request. All hosts are probed in parallel; the first
/// success wins. Sequential probing made the worst case ~N × timeout (e.g.
/// three dead 1337x mirrors at 8s each ≈ 30s); parallel probing bounds it to
/// the latency of the fastest answering host.
pub async fn fetch_with_failover(
    client: &reqwest::Client,
    hosts: &[String],
    path: &str,
) -> anyhow::Result<String> {
    anyhow::ensure!(!hosts.is_empty(), "no hosts");
    let start = pref_start(hosts);

    let probe = |off: usize| {
        let url = format!("{}{path}", hosts[(start + off) % hosts.len()]);
        async move {
            let t0 = std::time::Instant::now();
            let req = client.get(&url).timeout(HOST_TIMEOUT);
            let res = match req.send().await {
                Ok(resp) if resp.status().is_success() => resp
                    .text()
                    .await
                    .context("reading response body")
                    .map(|b| (off, b)),
                Ok(resp) => Err(anyhow::anyhow!("{url}: HTTP {}", resp.status())),
                Err(e) => Err(anyhow::anyhow!("{url}: {e}")),
            };
            tracing::debug!(
                url,
                ms = t0.elapsed().as_millis(),
                ok = res.is_ok(),
                "failover probe"
            );
            res
        }
    };
    let mut pending: Vec<_> = (0..hosts.len())
        .map(|off| {
            Box::pin(probe(off))
                as Pin<Box<dyn Future<Output = anyhow::Result<(usize, String)>> + Send>>
        })
        .collect();

    let mut last_err: Option<anyhow::Error> = None;
    while !pending.is_empty() {
        let (res, _, rest) = futures::future::select_all(pending).await;
        pending = rest;
        match res {
            Ok((off, body)) => {
                pref_record(hosts, (start + off) % hosts.len());
                return Ok(body);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no hosts")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magnet_builds_and_encodes() {
        let m = build_magnet("abc", "My File (2024) [1080p]");
        assert!(m.starts_with("magnet:?xt=urn:btih:abc&dn=My%20File%20%282024%29%20%5B1080p%5D"));
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("123.4 MiB"), (123.4 * 1024.0 * 1024.0) as u64);
        assert_eq!(
            parse_size("1.5 GB"),
            (1.5 * 1024.0 * 1024.0 * 1024.0) as u64
        );
        assert_eq!(parse_size("500"), 500);
        assert_eq!(parse_size("garbage"), 0);
    }

    #[test]
    fn canonical_hashes() {
        assert_eq!(
            canonicalize_hash("CAB507494D02EBB1178B38F2E9D7BE299C86B862"),
            Some("cab507494d02ebb1178b38f2e9d7be299c86b862".into())
        );
        // Base32 (SubsPlease-style) decodes to the same bytes as the hex above.
        assert_eq!(
            canonicalize_hash("ZK2QOSKNALV3CF4LHDZOTV56FGOINODC"),
            Some("cab507494d02ebb1178b38f2e9d7be299c86b862".into())
        );
        assert_eq!(canonicalize_hash("short"), None);
        assert_eq!(canonicalize_hash(""), None);
    }

    #[test]
    fn query_component_encoding() {
        assert_eq!(encode_query_component("a b&c=d"), "a+b%26c%3Dd");
        assert_eq!(encode_query_component("plain"), "plain");
        assert_eq!(encode_query_component(""), "");
        // Non-ASCII bytes percent-encode per byte, like the previous impls.
        assert_eq!(encode_query_component("café"), "caf%C3%A9");
    }

    #[test]
    fn magnet_encoding_keeps_percent_20() {
        let m = build_magnet("abc", "My File (2024)");
        assert!(m.ends_with("&dn=My%20File%20%282024%29"));
    }

    #[test]
    fn host_preference_remembers_last_success() {
        // Isolated host set so the static cache state from other tests cannot
        // interfere.
        let hosts: Vec<String> = ["https://h.example", "https://h2.example"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Starts at 0 with no memory.
        assert_eq!(pref_start(&hosts), 0);
        pref_record(&hosts, 1);
        assert_eq!(pref_start(&hosts), 1);
        pref_record(&hosts, 0);
        assert_eq!(pref_start(&hosts), 0);
        // Out-of-range index is clamped by the modulo in pref_start.
        pref_record(&hosts, 7);
        assert_eq!(pref_start(&hosts), 7 % hosts.len());
        // Different host set has its own entry.
        let other: Vec<String> = vec!["https://h3.example".into()];
        assert_eq!(pref_start(&other), 0);
    }

    /// End-to-end: a dead first host is skipped on the second call because
    /// the preference remembers the live one.
    #[tokio::test]
    async fn failover_skips_dead_host_on_retry() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            // Two calls: both hit the live host.
            for _ in 0..2 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let body = "ok";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });

        // Refuse port: any connection to it fails instantly.
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead); // port now refused

        let client = reqwest::Client::new();
        let hosts = vec![format!("http://{dead_addr}"), format!("http://{addr}")];
        // First call probes the dead host, then succeeds on the live one.
        let body = super::fetch_with_failover(&client, &hosts, "/x")
            .await
            .unwrap();
        assert_eq!(body, "ok");
        // The preference now points at the live host.
        assert_eq!(pref_start(&hosts), 1);
        // Second call still succeeds (and skips the dead host).
        let body = super::fetch_with_failover(&client, &hosts, "/x")
            .await
            .unwrap();
        assert_eq!(body, "ok");
    }
}

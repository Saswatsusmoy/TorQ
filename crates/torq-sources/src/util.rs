//! Small shared helpers: magnets, size/date parsing, host failover fetches.

use anyhow::Context;

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
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
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
pub async fn fetch_with_failover(
    client: &reqwest::Client,
    hosts: &[String],
    path: &str,
) -> anyhow::Result<String> {
    let mut last: Option<anyhow::Error> = None;
    for host in hosts {
        let url = format!("{host}{path}");
        let req = client.get(&url).timeout(std::time::Duration::from_secs(8));
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                return resp.text().await.context("reading response body");
            }
            Ok(resp) => last = Some(anyhow::anyhow!("{url}: HTTP {}", resp.status())),
            Err(e) => last = Some(anyhow::anyhow!("{url}: {e}")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no hosts")))
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
}

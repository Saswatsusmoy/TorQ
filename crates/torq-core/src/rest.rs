//! Client-side helpers for the daemon REST API, shared by the TUI and CLI:
//! "add this source, then play it the moment it becomes playable" without
//! duplicating the request plumbing in each client.

use std::time::Duration;

use reqwest::Client;

/// Add a torrent from a magnet (or base64 `.torrent` bytes) and return its
/// daemon-assigned numeric id. `auth` is the raw bearer token.
pub async fn add_torrent(
    client: &Client,
    base: &str,
    auth: &str,
    magnet: Option<&str>,
    torrent_b64: Option<&str>,
) -> anyhow::Result<usize> {
    let mut body = serde_json::Map::new();
    if let Some(m) = magnet {
        body.insert("magnet".into(), serde_json::Value::String(m.to_string()));
    }
    if let Some(b64) = torrent_b64 {
        body.insert("torrent_b64".into(), serde_json::Value::String(b64.to_string()));
    }
    let v: serde_json::Value = client
        .post(format!("{base}/torrents"))
        .bearer_auth(auth)
        .json(&serde_json::Value::Object(body))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    v["id"]
        .as_u64()
        .map(|i| i as usize)
        .ok_or_else(|| anyhow::anyhow!("add response missing id"))
}

/// Poll `/torrents/{id}/play` until the stream URL resolves — i.e. the
/// torrent's metadata is ready — or `timeout` elapses. `auth` is the raw
/// bearer token. A magnet's metadata
/// must come from the swarm, so this can take a few seconds; a `.torrent`
/// file resolves instantly.
pub async fn wait_playable(
    client: &Client,
    base: &str,
    auth: &str,
    id: usize,
    timeout: Duration,
) -> anyhow::Result<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let req = client
            .get(format!("{base}/torrents/{id}/play"))
            .bearer_auth(auth);
        match req.send().await {
            Ok(resp) => {
                if let Ok(v) = resp.json::<serde_json::Value>().await
                    && let Some(url) = v["url"].as_str()
                {
                    return Ok(url.to_string());
                }
            }
            Err(e) if tokio::time::Instant::now() >= deadline => {
                anyhow::bail!("no playable file yet ({e})");
            }
            Err(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "torrent {id} is not playable after {}s (no peers to fetch metadata from?)",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

/// Read a `.torrent` file and base64 it for the add API.
pub fn torrent_file_to_b64(path: &std::path::Path) -> anyhow::Result<String> {
    use base64::Engine;
    let bytes = std::fs::read(path)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

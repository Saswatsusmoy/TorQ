//! Background client: owns the HTTP conversation with the daemon.

use std::time::Duration;

use futures::StreamExt;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use torq_core::daemon::TorrentView;
use torq_sources::SearchReport;

/// UI → client requests.
#[derive(Debug)]
pub enum Action {
    Search(String),
    Add {
        magnet: String,
    },
    Pause(usize),
    Resume(usize),
    Remove {
        id: usize,
        delete_files: bool,
    },
    /// Resolve the torrent's stream URL and open it in a player.
    Play {
        id: usize,
    },
    /// Add a magnet, wait for it to become playable, then stream it — the
    /// one-key "play now" path (Stremio-style) for results not yet queued.
    AddAndPlay {
        magnet: String,
    },
    Refresh,
}

/// Client → UI state updates.
#[derive(Debug)]
pub enum UiMsg {
    Torrents(Vec<TorrentView>),
    /// Daemon-level config (queue slots) — refreshed with every snapshot.
    Config(ConfigInfo),
    Search(anyhow::Result<SearchReport>),
    Notice(String),
}

/// Subset of daemon config the UI renders.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ConfigInfo {
    pub max_active: usize,
}

pub struct Client {
    tx: UnboundedSender<Action>,
}

impl Client {
    pub fn spawn(
        base: String,
        token: String,
        player: Option<String>,
    ) -> (Self, UnboundedReceiver<UiMsg>) {
        let (action_tx, action_rx) = mpsc::unbounded_channel();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        tokio::spawn(client_loop(base, token, player, action_rx, msg_tx));
        (Self { tx: action_tx }, msg_rx)
    }

    pub fn send(&self, action: Action) {
        let _ = self.tx.send(action);
    }

    /// Test-only handle: wire a client to an app without starting the HTTP
    /// loop so key handlers can be exercised against captured actions.
    #[cfg(test)]
    pub fn for_test(tx: UnboundedSender<Action>) -> Self {
        Self { tx }
    }
}

/// Is the daemon answering /health? Any failure (connection refused, timeout,
/// bad status) means "not reachable" — the caller decides what to do.
pub async fn health(base: &str, token: &str) -> bool {
    let Ok(res) = reqwest::Client::new()
        .get(format!("{base}/health"))
        .bearer_auth(token)
        .timeout(Duration::from_secs(2))
        .send()
        .await
    else {
        return false;
    };
    res.status().is_success()
}

/// GET `builder` and decode its JSON body as `T`.
async fn get_json<T: serde::de::DeserializeOwned>(
    req: reqwest::RequestBuilder,
) -> anyhow::Result<T> {
    let resp = req.send().await?;
    let resp = resp.error_for_status()?;
    Ok(resp.json::<T>().await?)
}

async fn client_loop(
    base: String,
    token: String,
    player: Option<String>,
    mut actions: UnboundedReceiver<Action>,
    msgs: UnboundedSender<UiMsg>,
) {
    let http = reqwest::Client::new();
    let auth = format!("Bearer {token}");

    // SSE pings: any daemon event triggers a fresh torrents snapshot.
    let (ping_tx, mut ping_rx) = mpsc::channel::<()>(8);
    tokio::spawn(sse_loop(base.clone(), auth.clone(), ping_tx));

    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            Some(action) = actions.recv() => match action {
                Action::Search(q) => {
                    let req = http
                        .get(format!("{base}/search"))
                        .query(&[("q", q)])
                        .header("authorization", &auth);
                    let _ = msgs.send(UiMsg::Search(get_json::<SearchReport>(req).await));
                }
                Action::Add { magnet } => {
                    let res = http.post(format!("{base}/torrents"))
                        .header("authorization", &auth)
                        .json(&serde_json::json!({"magnet": magnet}))
                        .send().await;
                    if let Err(e) = res.and_then(|r| r.error_for_status()) {
                        let _ = msgs.send(UiMsg::Notice(format!("add failed: {e}")));
                    }
                    refresh(&http, &base, &auth, &msgs).await;
                }
                Action::Pause(id) => post_torrent(&http, &base, &auth, &msgs, id, "pause").await,
                Action::Resume(id) => post_torrent(&http, &base, &auth, &msgs, id, "resume").await,
                Action::Remove { id, delete_files } => {
                    let res = http.delete(format!("{base}/torrents/{id}"))
                        .query(&[("delete_files", delete_files)])
                        .header("authorization", &auth).send().await;
                    if let Err(e) = res.and_then(|r| r.error_for_status()) {
                        let _ = msgs.send(UiMsg::Notice(format!("remove failed: {e}")));
                    }
                    refresh(&http, &base, &auth, &msgs).await;
                }
                Action::Play { id } => play_torrent(&http, &base, &auth, &msgs, id, player.clone()).await,
                Action::AddAndPlay { magnet } => {
                    add_and_play(&http, &base, &token, &msgs, &player, &magnet).await;
                    refresh(&http, &base, &auth, &msgs).await;
                }
                Action::Refresh => refresh(&http, &base, &auth, &msgs).await,
            },
            _ = interval.tick() => refresh(&http, &base, &auth, &msgs).await,
            Some(()) = ping_rx.recv() => refresh(&http, &base, &auth, &msgs).await,
        }
    }
}

/// Add a magnet and launch the player the moment the stream URL resolves.
async fn add_and_play(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    msgs: &UnboundedSender<UiMsg>,
    player: &Option<String>,
    magnet: &str,
) {
    match torq_core::rest::add_torrent(http, base, token, Some(magnet), None).await {
        Ok(id) => {
            let _ = msgs.send(UiMsg::Notice(format!(
                "resolving metadata for torrent {id}…"
            )));
            match torq_core::rest::wait_playable(http, base, token, id, Duration::from_secs(120))
                .await
            {
                Ok(url) => match torq_core::player::open_in_player(&url, player.as_deref()) {
                    Ok(name) => {
                        let _ = msgs.send(UiMsg::Notice(format!("▶ streaming in {name}")));
                    }
                    Err(e) => {
                        let _ = msgs.send(UiMsg::Notice(format!("play failed: {e}")));
                    }
                },
                Err(e) => {
                    let _ = msgs.send(UiMsg::Notice(format!("not playable yet: {e}")));
                }
            }
        }
        Err(e) => {
            let _ = msgs.send(UiMsg::Notice(format!("add failed: {e}")));
        }
    }
}

async fn post_torrent(
    http: &reqwest::Client,
    base: &str,
    auth: &str,
    msgs: &UnboundedSender<UiMsg>,
    id: usize,
    action: &str,
) {
    let res = http
        .post(format!("{base}/torrents/{id}/{action}"))
        .header("authorization", auth)
        .send()
        .await;
    if let Err(e) = res.and_then(|r| r.error_for_status()) {
        let _ = msgs.send(UiMsg::Notice(format!("{action} failed: {e}")));
    }
    refresh(http, base, auth, msgs).await;
}

/// Ask the daemon for the playable stream URL and launch the OS player.
async fn play_torrent(
    http: &reqwest::Client,
    base: &str,
    auth: &str,
    msgs: &UnboundedSender<UiMsg>,
    id: usize,
    player: Option<String>,
) {
    let req = http
        .get(format!("{base}/torrents/{id}/play"))
        .header("authorization", auth);
    match get_json::<serde_json::Value>(req).await {
        Ok(play) => {
            if let Some(url) = play["url"].as_str() {
                // Prefer a real video player over the browser that
                // `open`/`xdg-open` would use for an http URL.
                match torq_core::player::open_in_player(url, player.as_deref()) {
                    Ok(name) => {
                        let _ = msgs.send(UiMsg::Notice(format!("▶ playing in {name}")));
                    }
                    Err(e) => {
                        let _ = msgs.send(UiMsg::Notice(format!("play failed: {e}")));
                    }
                }
            }
        }
        Err(e) => {
            let _ = msgs.send(UiMsg::Notice(format!("play failed: {e}")));
        }
    }
}

async fn refresh(http: &reqwest::Client, base: &str, auth: &str, msgs: &UnboundedSender<UiMsg>) {
    let req = http
        .get(format!("{base}/torrents"))
        .header("authorization", auth);
    match get_json::<Vec<TorrentView>>(req).await {
        Ok(views) => {
            let _ = msgs.send(UiMsg::Torrents(views));
        }
        Err(e) => {
            let _ = msgs.send(UiMsg::Notice(format!("daemon unreachable: {e}")));
        }
    }
    // Config rides along on the same refresh; failures are non-fatal — the
    // UI keeps its last-known queue slots.
    if let Ok(cfg) = get_json::<ConfigInfo>(
        http.get(format!("{base}/config")).header("authorization", auth),
    )
    .await
    {
        let _ = msgs.send(UiMsg::Config(cfg));
    }
}

/// Long-lived SSE subscription; every daemon event pings the main loop. On
/// disconnect, reconnect after a short backoff — the daemon may be restarting.
async fn sse_loop(base: String, auth: String, ping: mpsc::Sender<()>) {
    loop {
        let res = reqwest::Client::new()
            .get(format!("{base}/events"))
            .header("authorization", &auth)
            .send()
            .await;
        if let Ok(resp) = res {
            let mut stream = resp.bytes_stream();
            let mut line = Vec::new();
            'conn: loop {
                match stream.next().await {
                    Some(Ok(chunk)) => {
                        for &b in &chunk {
                            if b == b'\n' {
                                if line.starts_with(b"data:") {
                                    let _ = ping.send(()).await;
                                }
                                line.clear();
                            } else {
                                line.push(b);
                            }
                        }
                    }
                    _ => break 'conn,
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

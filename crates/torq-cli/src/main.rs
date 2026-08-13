//! torq — fast torrent finder and downloader.
//!
//! One binary, three faces: a long-lived daemon that owns the torrent engine
//! and serves the REST API, a TUI client (coming with the TUI phase), and CLI
//! verbs that talk to the daemon over its API.

use anyhow::Context;
use clap::{Parser, Subcommand};
use torq_core::api;
use torq_core::config::Config;
use torq_core::daemon::{Daemon, TorrentView};
use torq_core::engine::Engine;
use torq_core::{VERSION, watch};

mod update;

#[derive(Parser)]
#[command(
    name = "torq",
    version = VERSION,
    about = "Fast torrent finder and downloader",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the background daemon that owns the torrent engine.
    Daemon {
        /// Install the daemon as a login service (launchd / systemd user unit)
        /// and start it, then exit.
        #[arg(long)]
        install: bool,
    },
    /// Check for a newer release and update the binary.
    Update {
        /// Only report whether an update is available.
        #[arg(long)]
        check: bool,
    },
    /// Search all sources for a query (daemon must be running).
    Search { query: String },
    /// Add a magnet, infohash, or .torrent file to the daemon.
    Add { magnet: String },
    /// Show daemon health and download status.
    Status,
    /// Terminal UI (starts the daemon automatically if needed).
    Tui,
    /// RSS subscriptions: add feeds with filters, list, remove.
    Rss {
        #[command(subcommand)]
        cmd: RssCmd,
    },
    /// Print the stream URL for a torrent's video file (pipe to mpv/VLC).
    /// Accepts a torrent id, a magnet link, a 40-char infohash, or a path to
    /// a .torrent file (non-ids are added first and streamed once ready).
    Stream { id: String },
    /// Open the torrent's video in a real video player (streams while
    /// downloading). Accepts the same inputs as `stream`.
    Play { id: String },
    /// Cross-seed library: scan .torrent dirs, show index status.
    Library {
        #[command(subcommand)]
        cmd: LibraryCmd,
    },
    /// Set session rate limits live (bytes/sec; omit to clear).
    Limits {
        #[arg(long)]
        upload: Option<u32>,
        #[arg(long)]
        download: Option<u32>,
    },
}

#[derive(Subcommand)]
enum LibraryCmd {
    /// Scan library dirs for .torrent files (re-adding a match cross-seeds).
    Scan,
    /// Show how many torrents are indexed and from which dirs.
    Status,
}

#[derive(Subcommand)]
enum RssCmd {
    /// Subscribe to a feed (optionally filtered) and auto-download matches.
    Add {
        url: String,
        #[arg(long)]
        title_re: Option<String>,
        #[arg(long)]
        min_size: Option<u64>,
        #[arg(long)]
        max_size: Option<u64>,
        #[arg(long, default_value_t = 300)]
        interval: u64,
    },
    /// List subscriptions.
    List,
    /// Remove a subscription by id.
    Remove { id: u64 },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Daemon { install: true } => install_daemon(),
        Command::Daemon { install: false } => run_daemon().await,
        Command::Update { check } => {
            let out = if check {
                update::check().await?
            } else {
                update::update().await?
            };
            println!("{out}");
            Ok(())
        }
        Command::Search { query } => run_search(&query).await,
        Command::Add { magnet } => run_add(&magnet).await,
        Command::Status => run_status().await,
        Command::Tui => {
            let config = Config::load()?;
            torq_tui::run(&config).await
        }
        Command::Rss { cmd } => run_rss(cmd).await,
        Command::Stream { id } => run_stream(&id, false).await,
        Command::Play { id } => run_stream(&id, true).await,
        Command::Library { cmd } => run_library(cmd).await,
        Command::Limits { upload, download } => {
            let config = Config::load()?;
            let client = reqwest::Client::new();
            let base = format!("http://127.0.0.1:{}", config.api_port);
            let status = client
                .patch(format!("{base}/config/limits"))
                .bearer_auth(&config.auth_token)
                .json(&serde_json::json!({"upload_bps": upload, "download_bps": download}))
                .send()
                .await
                .with_context(|| "daemon not reachable — start it with `torq daemon`")?
                .status();
            anyhow::ensure!(status.is_success(), "limits failed: HTTP {status}");
            println!(
                "limits: up={} B/s down={} B/s",
                upload.unwrap_or(0),
                download.unwrap_or(0)
            );
            Ok(())
        }
    }
}

async fn run_library(cmd: LibraryCmd) -> anyhow::Result<()> {
    let config = Config::load()?;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", config.api_port);
    let method = match cmd {
        LibraryCmd::Scan => client.post(format!("{base}/library")),
        LibraryCmd::Status => client.get(format!("{base}/library")),
    };
    let status: serde_json::Value = method
        .bearer_auth(&config.auth_token)
        .send()
        .await
        .with_context(|| "daemon not reachable — start it with `torq daemon`")?
        .error_for_status()?
        .json()
        .await?;
    println!(
        "indexed {} torrent(s) from: {}",
        status["indexed"], status["dirs"]
    );
    Ok(())
}

/// Resolve the playable stream URL via the daemon's /play endpoint; with
/// `launch`, hand it to the OS player.
async fn run_stream(id: &str, launch: bool) -> anyhow::Result<()> {
    let config = Config::load()?;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", config.api_port);
    let auth = config.auth_token;

    // A numeric id addresses an existing torrent; anything else (magnet,
    // infohash, .torrent path) is added first and streamed once playable.
    let (id, added): (usize, bool) = match id.parse() {
        Ok(n) => (n, false),
        Err(_) => {
            let magnet = if id.starts_with("magnet:") {
                Some(id.to_string())
            } else if id.len() == 40 && id.chars().all(|c| c.is_ascii_hexdigit()) {
                Some(format!("magnet:?xt=urn:btih:{id}"))
            } else if std::path::Path::new(id).is_file() {
                None
            } else {
                anyhow::bail!("not a torrent id, magnet, infohash, or .torrent file: {id}");
            };
            let torrent_b64 = if magnet.is_none() {
                Some(torq_core::rest::torrent_file_to_b64(std::path::Path::new(id))?)
            } else {
                None
            };
            eprintln!("adding {id}…");
            let n = torq_core::rest::add_torrent(
                &client,
                &base,
                &auth,
                magnet.as_deref(),
                torrent_b64.as_deref(),
            )
            .await
            .with_context(|| "daemon not reachable — start it with `torq daemon`")?;
            eprintln!("waiting for metadata…");
            (n, true)
        }
    };

    // A freshly added torrent's metadata may still be resolving (magnets
    // fetch it from the swarm); poll until the stream URL exists.
    let (url, name, length) = if added {
        eprintln!("waiting for the stream to become playable…");
        let url = torq_core::rest::wait_playable(
            &client,
            &base,
            &auth,
            id,
            std::time::Duration::from_secs(120),
        )
        .await?;
        let play: serde_json::Value = client
            .get(format!("{base}/torrents/{id}/play"))
            .bearer_auth(&auth)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        (
            url,
            play["name"].as_str().unwrap_or("?").to_string(),
            play["length"].as_u64().unwrap_or(0),
        )
    } else {
        let play: serde_json::Value = client
            .get(format!("{base}/torrents/{id}/play"))
            .bearer_auth(&auth)
            .send()
            .await
            .with_context(|| "daemon not reachable — start it with `torq daemon`")?
            .error_for_status()?
            .json()
            .await?;
        (
            play["url"].as_str().unwrap_or_default().to_string(),
            play["name"].as_str().unwrap_or("?").to_string(),
            play["length"].as_u64().unwrap_or(0),
        )
    };
    println!("{url}");
    println!("  {name} ({length})");
    if launch {
        match torq_core::player::open_in_player(&url, config.player.as_deref()) {
            Ok(name) => println!("playing in {name}"),
            Err(e) => anyhow::bail!("{e}"),
        }
    }
    Ok(())
}

async fn run_rss(cmd: RssCmd) -> anyhow::Result<()> {
    let config = Config::load()?;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", config.api_port);
    let auth = |r: reqwest::RequestBuilder| r.bearer_auth(&config.auth_token);
    match cmd {
        RssCmd::Add {
            url,
            title_re,
            min_size,
            max_size,
            interval,
        } => {
            let sub: serde_json::Value =
                auth(client.post(format!("{base}/rss")).json(&serde_json::json!({
                    "url": url, "title_re": title_re, "min_size": min_size,
                    "max_size": max_size, "interval_secs": interval,
                })))
                .send()
                .await
                .with_context(|| "daemon not reachable — start it with `torq daemon`")?
                .error_for_status()?
                .json()
                .await?;
            println!("subscribed: {sub}");
        }
        RssCmd::List => {
            let subs: Vec<torq_core::rss::Subscription> = auth(client.get(format!("{base}/rss")))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            for s in &subs {
                println!(
                    "[{}] every {}s  {}  (filter: {})",
                    s.id,
                    s.interval_secs,
                    s.url,
                    s.title_re.as_deref().unwrap_or("-")
                );
            }
            if subs.is_empty() {
                println!("no subscriptions");
            }
        }
        RssCmd::Remove { id } => {
            let status = auth(client.delete(format!("{base}/rss/{id}")))
                .send()
                .await?
                .status();
            if status.is_success() {
                println!("removed subscription {id}");
            } else {
                anyhow::bail!("remove failed: HTTP {status}");
            }
        }
    }
    Ok(())
}

/// Install the daemon as a login service and start it. macOS: launchd agent;
/// Linux: systemd user unit. Prints the unit path and loads it.
fn install_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("resolving own binary")?;
    let label = "dev.torq.daemon";
    #[cfg(target_os = "macos")]
    {
        let dir = dirs::home_dir()
            .expect("home dir")
            .join("Library/LaunchAgents");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{label}.plist"));
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{}</string><string>daemon</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><true/>
</dict></plist>
"#,
            exe.display()
        );
        std::fs::write(&path, plist)?;
        std::process::Command::new("launchctl")
            .args(["load", path.to_str().expect("plist path")])
            .status()
            .context("launchctl load")?;
        println!("installed + loaded {}", path.display());
    }
    #[cfg(target_os = "linux")]
    {
        let dir = dirs::config_dir().expect("config dir").join("systemd/user");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("torq-daemon.service");
        let unit = format!(
            "[Unit]\nDescription=torq daemon\nAfter=network-online.target\n\n[Service]\nExecStart={} daemon\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
            exe.display()
        );
        std::fs::write(&path, unit)?;
        std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status()
            .context("systemctl daemon-reload")?;
        std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "torq-daemon.service"])
            .status()
            .context("systemctl enable")?;
        println!("installed + started {}", path.display());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    anyhow::bail!("service install is only supported on macOS and Linux");
    Ok(())
}

async fn run_daemon() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,torq=debug".into()),
        )
        .init();

    let config = Config::load()?;
    let engine = Engine::start(&config).await?;
    let daemon = Daemon::start(&config, engine).await?;
    watch::spawn_watchers(daemon.clone(), &config.watch_dirs)?;

    let sources = std::sync::Arc::new(torq_sources::Registry::all());
    let client = torq_sources::types::http_client(config.socks_proxy.as_deref())?;
    let app = api::router(
        daemon,
        config.auth_token.clone(),
        sources,
        client,
        config.api_port,
    );
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], config.api_port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "REST API listening");
    println!(
        "torq v{VERSION} daemon: http://{addr}  (token: {})",
        config.auth_token
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("shutting down");
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn run_status() -> anyhow::Result<()> {
    let config = Config::load()?;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", config.api_port);

    let health: serde_json::Value = client
        .get(format!("{base}/health"))
        .bearer_auth(&config.auth_token)
        .send()
        .await
        .with_context(|| "daemon not reachable — start it with `torq daemon`")?
        .json()
        .await?;
    let version = health["version"].as_str().unwrap_or("?");
    println!("torq daemon v{version} — {} torrent(s)", health["torrents"]);

    let views: Vec<TorrentView> = client
        .get(format!("{base}/torrents"))
        .bearer_auth(&config.auth_token)
        .send()
        .await?
        .json()
        .await?;

    if views.is_empty() {
        println!("  (no torrents)");
    }
    for v in &views {
        println!(
            "  [{:>2}] {:>11} {:5.1}% {:>10} {}",
            v.id,
            format!("{:?}", v.status).to_lowercase(),
            v.progress * 100.0,
            human_bytes(v.total_bytes),
            v.name
        );
    }
    Ok(())
}

/// Add a magnet, bare infohash, or path to a .torrent file.
async fn run_add(arg: &str) -> anyhow::Result<()> {
    let config = Config::load()?;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", config.api_port);
    let mut body = serde_json::json!({"magnet": arg, "paused": false});
    if std::path::Path::new(arg).is_file() {
        let bytes = std::fs::read(arg)?;
        body = serde_json::json!({"torrent_b64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)});
    }
    let view: torq_core::daemon::TorrentView = client
        .post(format!("{base}/torrents"))
        .bearer_auth(&config.auth_token)
        .json(&body)
        .send()
        .await
        .with_context(|| "daemon not reachable — start it with `torq daemon`")?
        .error_for_status()?
        .json()
        .await?;
    println!(
        "added [{}] {} ({:.1}%)",
        view.status_label(),
        view.name,
        view.progress * 100.0
    );
    Ok(())
}

async fn run_search(query: &str) -> anyhow::Result<()> {
    let config = Config::load()?;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", config.api_port);
    let report: torq_sources::SearchReport = client
        .get(format!("{base}/search"))
        .query(&[("q", query)])
        .bearer_auth(&config.auth_token)
        .send()
        .await
        .with_context(|| "daemon not reachable — start it with `torq daemon`")?
        .error_for_status()?
        .json()
        .await?;

    println!("{} result(s)", report.results.len());
    for r in report.results.iter().take(25) {
        println!(
            "{:>6}  {:>10}  {:24}  {}",
            r.seeders,
            human_bytes(r.size_bytes),
            r.source,
            r.name
        );
    }
    if !report.offline.is_empty() {
        println!("offline: {}", report.offline.join(", "));
    }
    Ok(())
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}

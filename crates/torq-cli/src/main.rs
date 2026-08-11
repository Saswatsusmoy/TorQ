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
use torq_core::{watch, VERSION};

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
    Daemon,
    /// Search all sources for a query (daemon must be running).
    Search { query: String },
    /// Show daemon health and download status.
    Status,
    /// Terminal UI (starts the daemon automatically if needed).
    Tui,
    /// RSS subscriptions: add feeds with filters, list, remove.
    Rss {
        #[command(subcommand)]
        cmd: RssCmd,
    },
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
        Command::Daemon => run_daemon().await,
        Command::Search { query } => run_search(&query).await,
        Command::Status => run_status().await,
        Command::Tui => {
            let config = Config::load()?;
            torq_tui::run(&config).await
        }
        Command::Rss { cmd } => run_rss(cmd).await,
    }
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
    let app = api::router(daemon, config.auth_token.clone(), sources, client);
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

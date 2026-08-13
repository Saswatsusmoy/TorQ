//! Terminal UI: a stateless ratatui client for the daemon's REST API.
//!
//! Attach/detach is just connect/disconnect — the daemon owns the engine and
//! keeps downloading while the TUI is closed. `torq tui` auto-starts the
//! daemon if it is not already running.

pub mod app;
pub mod format;
pub mod logo;
pub mod net;
pub mod theme;
pub mod ui;

use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result};
use torq_core::config::Config;

use crate::net::Client;

/// Run the TUI against the daemon described by `config`, starting the daemon
/// first if it is unreachable. Returns when the user quits; the daemon stays
/// up (that is the point of the split).
pub async fn run(config: &Config) -> Result<()> {
    let base = format!("http://127.0.0.1:{}", config.api_port);
    if !net::health(&base, &config.auth_token).await {
        spawn_daemon(config)?;
        for _ in 0..50 {
            if net::health(&base, &config.auth_token).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::ensure!(
            net::health(&base, &config.auth_token).await,
            "daemon did not become reachable at {base}"
        );
    }

    let (client, mut msgs) = Client::spawn(
        base.clone(),
        config.auth_token.clone(),
        config.player.clone(),
    );
    let mut terminal = ratatui::init();
    // Wordmark in the terminal chrome while the TUI owns the screen.
    crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle("torq"))
        .context("setting terminal title")?;
    std::io::stdout().flush().ok();
    let mut app = app::App::new(base);
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            app.tick();
            if crossterm::event::poll(Duration::from_millis(100))?
                && let crossterm::event::Event::Key(key) = crossterm::event::read()?
                && app.handle_key(key, &client).is_none()
            {
                break; // quit
            }
            while let Ok(msg) = msgs.try_recv() {
                app.apply(msg);
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

/// Launch the daemon as a detached child; its logs go to the state dir. The
/// child gets its own process group so terminal/SIGHUP teardown of the TUI
/// never takes the daemon down (that is the attach/detach contract).
fn spawn_daemon(config: &Config) -> Result<()> {
    let exe = std::env::current_exe().context("resolving own binary")?;
    let log = config.state_dir.join("daemon.log");
    let file =
        std::fs::File::create(&log).with_context(|| format!("creating {}", log.display()))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .stdout(std::process::Stdio::from(file.try_clone()?))
        .stderr(std::process::Stdio::from(file));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().context("spawning `torq daemon`")?;
    tracing::info!(pid = child.id(), "started daemon; log at {}", log.display());
    Ok(())
}

//! Watch folders: drop a `.torrent` file or a file containing a magnet link
//! and it starts downloading (torlink's `watch` mode, built into the daemon).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{recommended_watcher, EventKind, RecursiveMode, Watcher};
use tracing::{info, warn};

use crate::daemon::Daemon;

const ACCEPTED_EXTENSIONS: &[&str] = &["torrent", "magnet", "txt"];

/// Debounce window for the create/modify double-fire on most filesystems.
const DEBOUNCE: Duration = Duration::from_secs(2);

pub fn spawn_watchers(daemon: Arc<Daemon>, dirs: &[PathBuf]) -> Result<()> {
    for dir in dirs {
        spawn_watcher(daemon.clone(), dir.clone())?;
    }
    Ok(())
}

fn spawn_watcher(daemon: Arc<Daemon>, dir: PathBuf) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .context("creating filesystem watcher")?;
    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watching {}", dir.display()))?;
    // Daemon-scoped lifetime: the watcher lives for the whole process.
    std::mem::forget(watcher);

    tokio::spawn(async move {
        let (ttx, mut trx) = tokio::sync::mpsc::channel::<PathBuf>(64);
        tokio::task::spawn_blocking(move || {
            while let Ok(res) = rx.recv() {
                let Ok(event) = res else { continue };
                if matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                    for path in event.paths {
                        if is_accepted(&path) {
                            let _ = ttx.blocking_send(path);
                        }
                    }
                }
            }
        });
        let mut last: Option<(PathBuf, Instant)> = None;
        while let Some(path) = trx.recv().await {
            if let Some((prev, at)) = &last {
                if prev == &path && at.elapsed() < DEBOUNCE {
                    continue;
                }
            }
            last = Some((path.clone(), Instant::now()));
            if let Err(e) = ingest(daemon.clone(), &path).await {
                warn!(path = %path.display(), "watch ingest failed: {e:#}");
            }
        }
    });
    info!(dir = %dir.display(), "watching for torrents");
    Ok(())
}

fn is_accepted(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| ACCEPTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

async fn ingest(daemon: Arc<Daemon>, path: &Path) -> Result<()> {
    let bytes = tokio::fs::read(path).await?;
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = text.trim();

    if let Some(magnet) = trimmed.strip_prefix("magnet:?") {
        // Keep the full magnet; strip_prefix only validated the prefix.
        let full = format!("magnet:?{magnet}");
        let view = daemon.add_magnet(&full, false).await?;
        info!(info_hash = %view.info_hash, "watch: added magnet from {}", path.display());
        return Ok(());
    }
    if trimmed.len() == 40 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        let view = daemon.add_magnet(trimmed, false).await?;
        info!(info_hash = %view.info_hash, "watch: added infohash from {}", path.display());
        return Ok(());
    }
    // Fall back to treating it as a .torrent file (binary).
    let view = daemon.add_torrent_bytes(bytes, false).await?;
    info!(info_hash = %view.info_hash, "watch: added torrent from {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_expected_extensions() {
        assert!(is_accepted(Path::new("/tmp/x.torrent")));
        assert!(is_accepted(Path::new("/tmp/x.MAGNET")));
        assert!(is_accepted(Path::new("/tmp/x.txt")));
        assert!(!is_accepted(Path::new("/tmp/x.mp4")));
        assert!(!is_accepted(Path::new("/tmp/x")));
    }
}

//! Self-update: manifest-checked binary replacement with an atomic swap.
//!
//! The manifest is a small JSON (`{ "version": "x.y.z", "url": "<binary>" }`)
//! served from `TORQ_UPDATE_URL` (see README). The new binary is downloaded to
//! a temp file next to the current one and renamed over it — atomic on the
//! same filesystem.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::VERSION;

#[derive(Deserialize)]
struct Manifest {
    version: String,
    url: String,
}

pub fn manifest_url() -> String {
    std::env::var("TORQ_UPDATE_URL").unwrap_or_else(|_| {
        "https://github.com/torq-app/torq/releases/latest/download/manifest.json".into()
    })
}

/// Fetch the manifest and report whether a newer version exists.
pub async fn check() -> Result<String> {
    let manifest = fetch_manifest().await?;
    if manifest.version == VERSION {
        Ok(format!("already up to date (v{VERSION})"))
    } else {
        Ok(format!(
            "update available: v{VERSION} -> v{}",
            manifest.version
        ))
    }
}

/// Download and atomically replace the running binary.
pub async fn update() -> Result<String> {
    let manifest = fetch_manifest().await?;
    if manifest.version == VERSION {
        return Ok(format!("already up to date (v{VERSION})"));
    }
    let exe = std::env::current_exe().context("resolving own binary path")?;
    let tmp = exe.with_extension("new");
    let bytes = reqwest::get(&manifest.url)
        .await
        .context("downloading update")?
        .error_for_status()
        .context("update download failed")?
        .bytes()
        .await
        .context("reading update body")?;
    std::fs::write(&tmp, &bytes).context("writing update")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &exe).context("swapping binary")?;
    Ok(format!("updated to v{}", manifest.version))
}

async fn fetch_manifest() -> Result<Manifest> {
    let url = manifest_url();
    let resp = reqwest::get(&url)
        .await
        .with_context(|| format!("fetching {url}"))?
        .error_for_status()
        .with_context(|| format!("manifest at {url} returned an error"))?;
    resp.json().await.context("parsing manifest")
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn update_swaps_binary_atomically() {
        let dir = std::env::temp_dir().join(format!("torq-upd-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("torq");
        std::fs::write(&exe, "old").unwrap();
        let tmp = exe.with_extension("new");
        std::fs::write(&tmp, "new-bytes").unwrap();
        std::fs::rename(&tmp, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new-bytes");
        assert!(!tmp.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}

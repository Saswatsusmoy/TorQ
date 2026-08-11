//! Self-update: manifest-checked binary replacement with an atomic swap.
//!
//! The manifest is a small JSON served from `TORQ_UPDATE_URL` (defaults to the
//! GitHub release asset): `{ "version": "x.y.z", "platforms": { "<target>": "<url>" } }`
//! keyed by rust target triple. The new binary is downloaded to a temp file
//! next to the current one and renamed over it — atomic on the same filesystem.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::VERSION;

#[derive(Deserialize)]
struct Manifest {
    version: String,
    /// Binary URLs keyed by rust target triple.
    platforms: HashMap<String, String>,
}

pub fn manifest_url() -> String {
    std::env::var("TORQ_UPDATE_URL").unwrap_or_else(|_| {
        "https://github.com/Saswatsusmoy/TorQ/releases/latest/download/manifest.json".into()
    })
}

/// The rust target triple for the running binary.
fn current_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        (os, arch) => Box::leak(format!("{arch}-unknown-{os}").into_boxed_str()),
    }
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
    let url = manifest
        .platforms
        .get(current_target())
        .context("no release binary for this platform")?
        .clone();
    let exe = std::env::current_exe().context("resolving own binary path")?;
    let tmp = exe.with_extension("new");
    let bytes = reqwest::get(&url)
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
    use super::*;

    #[test]
    fn manifest_platform_picks_current_target() {
        let mut platforms = HashMap::new();
        platforms.insert("aarch64-apple-darwin".into(), "url-a".to_string());
        platforms.insert("x86_64-unknown-linux-gnu".into(), "url-b".to_string());
        let manifest = Manifest {
            version: "9.9.9".into(),
            platforms,
        };
        let url = manifest.platforms.get(current_target()).unwrap();
        let expected = if current_target() == "aarch64-apple-darwin" {
            "url-a"
        } else {
            "url-b"
        };
        assert_eq!(url, expected);
    }
}

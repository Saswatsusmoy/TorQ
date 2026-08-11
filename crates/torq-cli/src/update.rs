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

/// Download and atomically replace the running binary. Release assets are
/// `torq-<target>.tar.gz` (same archives install.sh and Homebrew use), so the
/// `torq` entry is extracted before the swap — writing the archive raw would
/// produce a corrupt binary.
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
    let archive = reqwest::get(&url)
        .await
        .context("downloading update")?
        .error_for_status()
        .context("update download failed")?
        .bytes()
        .await
        .context("reading update body")?;
    let binary = extract_torq(&archive).with_context(|| format!("extracting update from {url}"))?;

    let exe = std::env::current_exe().context("resolving own binary path")?;
    let tmp = exe.with_extension("new");
    std::fs::write(&tmp, &binary).context("writing update")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &exe).context("swapping binary")?;
    Ok(format!("updated to v{}", manifest.version))
}

/// Pull the `torq` entry out of a `torq-<target>.tar.gz` archive.
fn extract_torq(archive: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let gz = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(gz);
    for entry in tar.entries().context("reading archive")? {
        let mut entry = entry.context("reading archive entry")?;
        if entry
            .path()
            .context("reading entry path")?
            .ends_with("torq")
        {
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    anyhow::bail!("archive contains no torq binary")
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

    #[test]
    fn extract_torq_pulls_binary_from_archive() {
        // Build a real torq-<target>.tar.gz in memory and extract it back.
        let mut tar_bytes = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut tar_bytes, flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(5);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "torq", std::io::Cursor::new(b"hello"))
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let extracted = extract_torq(&tar_bytes).unwrap();
        assert_eq!(extracted, b"hello");
    }

    #[test]
    fn extract_torq_rejects_missing_binary() {
        let mut tar_bytes = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut tar_bytes, flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "readme.txt", std::io::Cursor::new(b"x"))
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        assert!(extract_torq(&tar_bytes).is_err());
    }
}

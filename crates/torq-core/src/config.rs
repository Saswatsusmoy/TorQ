//! Persistent daemon configuration.
//!
//! Plain TOML at `~/.config/torq/config.toml` (macOS: `~/Library/Application
//! Support/torq/config.toml`). The auth token is generated on first run and
//! lives here so local clients (TUI/CLI) can read it without user setup.

use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::APP_NAME;

fn default_download_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| dirs::home_dir().expect("home dir"))
}

fn default_state_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| dirs::home_dir().expect("home dir"))
        .join(APP_NAME)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where finished downloads land.
    pub download_dir: PathBuf,
    /// Where the engine session, queue, and library index live.
    pub state_dir: PathBuf,
    /// Extra tracker URLs announced for every torrent.
    pub trackers: Vec<String>,
    /// Global upload ceiling in bytes/sec (None = unlimited).
    pub upload_bps: Option<u32>,
    /// Global download ceiling in bytes/sec (None = unlimited).
    pub download_bps: Option<u32>,
    /// SOCKS5 proxy URL for all outbound traffic (trackers, peers, sources).
    pub socks_proxy: Option<String>,
    /// Bearer token for the REST API. Generated on first run.
    pub auth_token: String,
    /// Port for the REST API (bound to 127.0.0.1 only).
    pub api_port: u16,
    /// Folders watched for dropped .torrent / magnet files.
    pub watch_dirs: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            state_dir: default_state_dir(),
            trackers: Vec::new(),
            upload_bps: None,
            download_bps: None,
            socks_proxy: None,
            auth_token: generate_token(),
            api_port: 8170,
            watch_dirs: Vec::new(),
        }
    }
}

impl Config {
    pub fn config_file() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| dirs::home_dir().expect("home dir"))
            .join(APP_NAME)
            .join("config.toml")
    }

    /// Load config, creating and persisting defaults (with a fresh auth token)
    /// if the file is absent — clients need the token on disk.
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::config_file();
        let cfg = match fs::read_to_string(&path) {
            Ok(raw) => toml::from_str::<Self>(&raw)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(_) => {
                let cfg = Self::default();
                cfg.save()?;
                cfg
            }
        };
        fs::create_dir_all(&cfg.state_dir)
            .with_context(|| format!("creating state dir {}", cfg.state_dir.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::config_file();
        fs::create_dir_all(path.parent().expect("config parent"))
            .with_context(|| format!("creating config dir {}", path.display()))?;
        let raw = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))
    }
}

fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("OS randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roundtrips() {
        let cfg = Config::default();
        let raw = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&raw).unwrap();
        assert_eq!(back.auth_token, cfg.auth_token);
        assert_eq!(back.download_dir, cfg.download_dir);
    }

    #[test]
    fn missing_token_is_generated() {
        let raw = "download_dir = \"/tmp/dl\"\n";
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.auth_token.len(), 32);
        assert_ne!(cfg.auth_token, Config::default().auth_token);
    }
}

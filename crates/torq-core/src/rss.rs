//! RSS subscriptions: a feed URL plus filters, polled on a jittered cadence;
//! matching items are added to the download queue automatically.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::daemon::Daemon;
use torq_sources::rss_src::{RssDef, RssSource};
use torq_sources::{Source, TorrentResult};

fn default_interval() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub title_re: Option<String>,
    #[serde(default)]
    pub min_size: Option<u64>,
    #[serde(default)]
    pub max_size: Option<u64>,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(skip)]
    pub next_poll: u64,
    #[serde(skip)]
    pub seen: Vec<String>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
struct State {
    subs: Vec<Subscription>,
    next_id: u64,
}

pub struct Subscriptions {
    path: PathBuf,
    inner: Mutex<State>,
}

impl Subscriptions {
    pub fn load(path: PathBuf) -> Arc<Self> {
        let state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<State>(&raw).ok())
            .unwrap_or_default();
        let next_id = state.subs.iter().map(|s| s.id).max().map_or(1, |m| m + 1);
        Arc::new(Self {
            path,
            inner: Mutex::new(State { next_id, ..state }),
        })
    }

    fn save(&self) {
        let state = self.inner.lock().clone();
        if let Err(e) = std::fs::write(&self.path, serde_json::to_vec(&state).unwrap_or_default()) {
            warn!(path = %self.path.display(), "subscriptions write failed: {e}");
        }
    }

    pub fn list(&self) -> Vec<Subscription> {
        self.inner.lock().subs.clone()
    }

    /// Add a subscription; validates URL and filter regex up front.
    pub fn add(
        &self,
        url: &str,
        title_re: Option<String>,
        min_size: Option<u64>,
        max_size: Option<u64>,
        interval_secs: u64,
    ) -> Result<Subscription> {
        url::Url::parse(url).context("invalid feed URL")?;
        if let Some(re) = &title_re {
            Regex::new(re).context("invalid title regex")?;
        }
        let mut state = self.inner.lock();
        let sub = Subscription {
            id: state.next_id,
            url: url.to_string(),
            title_re,
            min_size,
            max_size,
            interval_secs,
            next_poll: 0, // poll on the next tick
            seen: Vec::new(),
        };
        state.next_id += 1;
        state.subs.push(sub.clone());
        drop(state);
        self.save();
        Ok(sub)
    }

    /// Remove by id; returns false when no such subscription exists.
    pub fn remove(&self, id: u64) -> bool {
        let mut state = self.inner.lock();
        let before = state.subs.len();
        state.subs.retain(|s| s.id != id);
        let removed = state.subs.len() != before;
        drop(state);
        if removed {
            self.save();
        }
        removed
    }

    /// Poll every subscription whose next-poll time has passed.
    pub async fn poll_due(&self, client: &reqwest::Client, daemon: &Daemon) {
        let now = now();
        let due: Vec<Subscription> = self
            .inner
            .lock()
            .subs
            .iter()
            .filter(|s| s.next_poll <= now)
            .cloned()
            .collect();
        for sub in due {
            self.poll_one(&sub, client, daemon).await;
        }
    }

    async fn poll_one(&self, sub: &Subscription, client: &reqwest::Client, daemon: &Daemon) {
        let source = RssSource::new(RssDef {
            id: format!("sub:{}", sub.id),
            label: "subscription".into(),
            hosts: vec![sub.url.clone()],
            ..Default::default()
        });
        let items = match source.search("", client).await {
            Ok(items) => items,
            Err(e) => {
                debug!(sub = sub.id, %sub.url, "poll failed: {e:#}");
                return;
            }
        };
        let re = sub.title_re.as_ref().and_then(|r| Regex::new(r).ok());

        // Decide what to add and update bookkeeping under the lock; the
        // network calls happen after it is released.
        let to_add: Vec<TorrentResult> = {
            let mut state = self.inner.lock();
            let Some(s) = state.subs.iter_mut().find(|s| s.id == sub.id) else {
                return;
            };
            let mut to_add = Vec::new();
            for item in items {
                if s.seen.iter().any(|h| h == &item.info_hash) {
                    continue;
                }
                s.seen.push(item.info_hash.clone());
                if matches_filters(re.as_ref(), s, &item) {
                    to_add.push(item);
                }
            }
            s.seen.truncate(500);
            // Stagger polls so a burst of subscriptions doesn't all fire at once.
            s.next_poll = now() + s.interval_secs + jitter(&s.url);
            to_add
        };
        self.save();

        let mut added = 0usize;
        for item in to_add {
            match daemon.add_magnet(&item.magnet, false).await {
                Ok(_) => {
                    added += 1;
                    info!(sub = sub.id, hash = %item.info_hash, name = %item.name, "autodownloaded");
                }
                Err(e) => warn!(sub = sub.id, hash = %item.info_hash, "autodownload failed: {e:#}"),
            }
        }
        debug!(sub = sub.id, added, "polled {}", sub.url);
    }
}

/// Filter rules: optional title regex, optional size window. Items with
/// unknown size (0) fail any size rule — they cannot be verified.
fn matches_filters(re: Option<&Regex>, sub: &Subscription, item: &TorrentResult) -> bool {
    if let Some(re) = re
        && !re.is_match(&item.name)
    {
        return false;
    }
    if let Some(min) = sub.min_size
        && (item.size_bytes == 0 || item.size_bytes < min)
    {
        return false;
    }
    if let Some(max) = sub.max_size
        && (item.size_bytes == 0 || item.size_bytes > max)
    {
        return false;
    }
    true
}

/// Deterministic sub-minute stagger derived from the feed URL.
fn jitter(url: &str) -> u64 {
    (url.bytes().fold(0u64, |a, b| a.wrapping_add(b as u64)) % 60) + 1
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(title_re: Option<String>, min: Option<u64>, max: Option<u64>) -> Subscription {
        Subscription {
            id: 1,
            url: "https://x".into(),
            title_re,
            min_size: min,
            max_size: max,
            interval_secs: 60,
            next_poll: 0,
            seen: vec![],
        }
    }

    fn item(name: &str, size: u64) -> TorrentResult {
        TorrentResult {
            info_hash: "hash".into(),
            name: name.into(),
            size_bytes: size,
            seeders: 0,
            leechers: 0,
            num_files: None,
            source: "test".into(),
            magnet: "magnet:?xt=urn:btih:hash".into(),
            added: None,
        }
    }

    #[test]
    fn filters_match_regex_and_size() {
        let s = sub(
            Some(r"1080p".into()),
            Some(1_000_000_000),
            Some(5_000_000_000),
        );
        let re = Regex::new(s.title_re.as_ref().unwrap()).ok();
        assert!(matches_filters(
            re.as_ref(),
            &s,
            &item("Show 01 [1080p]", 2_000_000_000)
        ));
        assert!(!matches_filters(
            re.as_ref(),
            &s,
            &item("Show 01 [720p]", 2_000_000_000)
        ));
        assert!(!matches_filters(
            re.as_ref(),
            &s,
            &item("Show 01 [1080p]", 500_000_000)
        )); // below min
        assert!(!matches_filters(
            re.as_ref(),
            &s,
            &item("Show 01 [1080p]", 6_000_000_000)
        )); // above max
    }

    #[test]
    fn unknown_size_fails_size_rules() {
        let s = sub(None, Some(100), None);
        assert!(!matches_filters(None, &s, &item("x", 0)));
        assert!(matches_filters(None, &s, &item("x", 200)));
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = std::env::temp_dir().join(format!("torq-rss-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("subs.json");
        let subs = Subscriptions::load(path.clone());
        subs.add(
            "https://nyaa.si/?page=rss&q=test",
            Some("1080".into()),
            None,
            None,
            60,
        )
        .unwrap();
        subs.remove(0); // no-op, wrong id
        let reloaded = Subscriptions::load(path.clone());
        let list = reloaded.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title_re.as_deref(), Some("1080"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn add_rejects_bad_regex() {
        let dir = std::env::temp_dir().join(format!("torq-rss-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let subs = Subscriptions::load(dir.join("subs.json"));
        assert!(
            subs.add("https://nyaa.si", Some("([".into()), None, None, 60)
                .is_err()
        );
        assert!(subs.add("not a url", None, None, None, 60).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}

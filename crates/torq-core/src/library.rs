//! Cross-seed library index: `.torrent` files on disk whose data is already
//! downloaded. Re-adding a matching infohash points librqbit at the existing
//! files (`output_folder` = torrent file's parent dir), so the piece check
//! finds them instead of re-downloading.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use buffers::ByteBufOwned;
use librqbit_core::torrent_metainfo::torrent_from_bytes;
use parking_lot::Mutex;
use tracing::{debug, info};

#[derive(Debug, Clone)]
pub struct LibraryEntry {
    pub torrent_path: PathBuf,
    pub data_dir: PathBuf,
    pub total_bytes: u64,
}

pub struct Library {
    dirs: Vec<PathBuf>,
    index: Mutex<HashMap<String, LibraryEntry>>,
}

impl Library {
    pub fn new(dirs: Vec<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            dirs,
            index: Mutex::new(HashMap::new()),
        })
    }

    /// Rebuild the index from `dirs`; returns the number of torrents found.
    pub fn scan(&self) -> Result<usize> {
        let mut index = HashMap::new();
        for dir in &self.dirs {
            walk(&mut index, dir)?;
        }
        let n = index.len();
        *self.index.lock() = index;
        info!(count = n, "library scan complete");
        Ok(n)
    }

    pub fn lookup(&self, hash: &str) -> Option<LibraryEntry> {
        self.index.lock().get(hash).cloned()
    }

    pub fn count(&self) -> usize {
        self.index.lock().len()
    }

    pub fn dirs(&self) -> Vec<PathBuf> {
        self.dirs.clone()
    }
}

fn walk(index: &mut HashMap<String, LibraryEntry>, dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk(index, &path)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("torrent") {
            if let Err(e) = index_torrent(index, &path) {
                debug!(path = %path.display(), "skipping torrent: {e:#}");
            }
        }
    }
    Ok(())
}

fn index_torrent(index: &mut HashMap<String, LibraryEntry>, path: &Path) -> Result<()> {
    let bytes = std::fs::read(path)?;
    let meta = torrent_from_bytes::<ByteBufOwned>(&bytes).context("parsing")?;
    let hash = meta.info_hash.as_string();
    let total: u64 = meta.info.iter_file_lengths()?.sum();
    let data_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    index.insert(
        hash,
        LibraryEntry {
            torrent_path: path.to_path_buf(),
            data_dir,
            total_bytes: total,
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Minimal single-file torrent: info = {length, name, piece length, pieces}.
    fn bencode_torrent() -> Vec<u8> {
        let mut info = b"d6:lengthi1000e4:name8:data.bin12:piece lengthi16384e6:pieces20:".to_vec();
        info.extend(std::iter::repeat_n(0u8, 20));
        info.push(b'e');
        let mut out = b"d8:announce12:https://x.io4:info".to_vec();
        out.extend_from_slice(&info);
        out.push(b'e');
        out
    }

    #[test]
    fn scan_indexes_and_lookup_returns_entry() {
        let dir = std::env::temp_dir().join(format!("torq-lib-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join("t.torrent")).unwrap();
        f.write_all(&bencode_torrent()).unwrap();

        let lib = Library::new(vec![dir.clone()]);
        assert_eq!(lib.scan().unwrap(), 1);
        let raw = bencode_torrent();
        let parsed = torrent_from_bytes::<ByteBufOwned>(&raw).unwrap();
        let entry = lib.lookup(&parsed.info_hash.as_string()).unwrap();
        assert_eq!(entry.data_dir, dir);
        assert_eq!(entry.total_bytes, 1000);
        assert!(entry.torrent_path.ends_with("t.torrent"));

        std::fs::remove_dir_all(&dir).ok();
    }
}

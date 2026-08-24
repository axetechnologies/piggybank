use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

mod json;
mod markers;
mod session;
mod text;
pub use json::{compress_json, decompress_json};
pub use session::Session;
pub use text::{compress_text, decompress_text, TextOptions};

/// Content-addressed blob store. Every write is keyed by the sha256 of its
/// bytes, so writing the same content twice is a no-op and nothing already
/// on disk is ever overwritten. This is the one primitive that makes
/// compression reversible: a compressor never has to decide what's safe to
/// throw away, because nothing it stores here is ever thrown away.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn put(&self, bytes: &[u8]) -> std::io::Result<String> {
        let id = hex::encode(Sha256::digest(bytes));
        let path = self.root.join(&id);
        if !path.exists() {
            fs::write(&path, bytes)?;
        }
        Ok(id)
    }

    pub fn get(&self, id: &str) -> std::io::Result<Vec<u8>> {
        fs::read(self.root.join(id))
    }

    /// Count entries and total bytes actually held in the store. Filters to
    /// files whose names look like our sha256-hex ids so sidecar files
    /// living in the same directory (e.g. `Session`'s `.session.json`)
    /// don't get counted as content.
    pub fn stats(&self) -> std::io::Result<StoreStats> {
        let mut entries = 0usize;
        let mut bytes = 0u64;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let is_content_id = name
                .to_str()
                .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()));
            if is_content_id && entry.file_type()?.is_file() {
                entries += 1;
                bytes += entry.metadata()?.len();
            }
        }
        Ok(StoreStats { entries, bytes })
    }
}

pub struct StoreStats {
    pub entries: usize,
    pub bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_put_get_round_trips_and_dedupes() {
        let dir = std::env::temp_dir().join(format!("boomerang-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();
        let id1 = store.put(b"hello world").unwrap();
        let id2 = store.put(b"hello world").unwrap(); // identical content -> identical id, no rewrite
        assert_eq!(id1, id2);
        assert_eq!(store.get(&id1).unwrap(), b"hello world");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn store_stats_counts_content_ignores_non_id_files() {
        let dir = std::env::temp_dir().join(format!("boomerang-stats-test-{}", std::process::id()));
        let store = Store::open(&dir).unwrap();
        store.put(b"hello").unwrap();
        store.put(b"world!!").unwrap();
        store.put(b"hello").unwrap(); // dedup: same id as the first, not a third entry
        fs::write(dir.join(".session.json"), b"{}").unwrap(); // sidecar, must be ignored

        let stats = store.stats().unwrap();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.bytes, 5 + 7);
        fs::remove_dir_all(&dir).ok();
    }
}

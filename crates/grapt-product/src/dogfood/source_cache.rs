//! SC-PERF-01a/03: bounded source snapshots, invalidated by current file metadata.
//! Apache-2.0): retain validated.
//! snapshots, invalidate before refresh, and never keep a lock across async I/O.
use grapt_config::{
    ConfigBounds, ConfigSourcePath, ConfigValue, MaxDocumentBytes, MaxPathBytes, SourceReader,
    StdSourceReader,
};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

const MAX_FILES: usize = 256;
const MAX_BYTES: usize = 4 * 1024 * 1024;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

#[derive(PartialEq, Eq)]
struct Stamp {
    target: PathBuf,
    modified: SystemTime,
    len: u64,
    readonly: bool,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64, u32),
    #[cfg(windows)]
    created: u64,
}
impl Stamp {
    fn read(path: &Path, bound: usize) -> Result<Option<Self>, String> {
        let metadata = match std::fs::metadata(path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("source metadata is unreadable: {error}")),
        };
        #[cfg(target_os = "linux")]
        if metadata.is_dir() && grapt_sandbox::Bwrap::synthetic_directory(path)? {
            return Ok(None);
        }
        if !metadata.is_file() {
            return Err("source is not a bounded regular file".into());
        }
        if metadata.len() > bound as u64 {
            return Err("source is not a bounded regular file: exceeds its file bound".into());
        }
        Ok(Some(Self {
            target: std::fs::canonicalize(path).map_err(|error| error.to_string())?,
            modified: metadata.modified().map_err(|error| error.to_string())?,
            len: metadata.len(),
            readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                    metadata.mode(),
                )
            },
            #[cfg(windows)]
            created: std::os::windows::fs::MetadataExt::creation_time(&metadata),
        }))
    }
}
struct Entry {
    stamp: Stamp,
    bytes: Arc<[u8]>,
    parsed: Option<(ConfigBounds, ConfigValue)>,
}
#[derive(Default)]
struct Cache {
    entries: BTreeMap<PathBuf, Entry>,
    order: VecDeque<PathBuf>,
    bytes: usize,
}
impl Cache {
    fn remove(&mut self, path: &Path) {
        if let Some(entry) = self.entries.remove(path) {
            self.bytes -= entry.bytes.len();
        }
        self.order.retain(|held| held != path);
    }
    fn load(&mut self, path: &Path, bound: MaxDocumentBytes) -> Result<Option<Arc<[u8]>>, String> {
        self.load_with(path, bound, |source, bound| {
            StdSourceReader
                .read_bounded(source, bound)
                .map_err(|error| error.to_string())
        })
    }
    fn load_with(
        &mut self,
        path: &Path,
        bound: MaxDocumentBytes,
        read: impl FnOnce(&ConfigSourcePath, MaxDocumentBytes) -> Result<Vec<u8>, String>,
    ) -> Result<Option<Arc<[u8]>>, String> {
        let stamp = match Stamp::read(path, bound.value()) {
            Ok(Some(stamp)) => stamp,
            other => {
                self.remove(path);
                return other.map(|_| None);
            }
        };
        if self
            .entries
            .get(path)
            .is_some_and(|entry| entry.stamp == stamp)
        {
            // ACL changes need not change mtime or length. Revalidate read
            // access on a hit without rereading or parsing the document.
            if let Err(error) = std::fs::File::open(path) {
                self.remove(path);
                return Err(format!("source is unreadable: {error}"));
            }
            return Ok(self.entries.get(path).map(|entry| Arc::clone(&entry.bytes)));
        }
        self.remove(path);
        let source = ConfigSourcePath::new(
            path,
            MaxPathBytes::try_new(4096).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let bytes: Arc<[u8]> = read(&source, bound)?.into();
        if bytes.len() > bound.value() {
            return Err("source is not a bounded regular file: exceeds its file bound".into());
        }
        if Stamp::read(path, bound.value())?.as_ref() != Some(&stamp) {
            return Err("source changed while being read".into());
        }
        if bytes.len() <= MAX_BYTES {
            while self.entries.len() >= MAX_FILES || self.bytes + bytes.len() > MAX_BYTES {
                let Some(oldest) = self.order.pop_front() else {
                    break;
                };
                if let Some(entry) = self.entries.remove(&oldest) {
                    self.bytes -= entry.bytes.len();
                }
            }
            self.bytes += bytes.len();
            self.order.push_back(path.to_owned());
            self.entries.insert(
                path.to_owned(),
                Entry {
                    stamp,
                    bytes: Arc::clone(&bytes),
                    parsed: None,
                },
            );
        }
        Ok(Some(bytes))
    }
}
pub(super) fn document(
    path: &Path,
    bounds: ConfigBounds,
) -> Result<Option<(usize, ConfigValue)>, String> {
    let mut cache = CACHE
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "source cache lock poisoned")?;
    let Some(bytes) = cache.load(path, bounds.document_bytes())? else {
        return Ok(None);
    };
    if let Some((_, value)) = cache
        .entries
        .get(path)
        .and_then(|entry| entry.parsed.as_ref())
        .filter(|(held, _)| *held == bounds)
    {
        return Ok(Some((bytes.len(), value.clone())));
    }
    let value = grapt_config::decode(&bytes, bounds)
        .map_err(|error| error.to_string())?
        .into_value();
    if let Some(entry) = cache.entries.get_mut(path) {
        entry.parsed = Some((bounds, value.clone()));
    }
    Ok(Some((bytes.len(), value)))
}
#[cfg(test)]
mod tests;

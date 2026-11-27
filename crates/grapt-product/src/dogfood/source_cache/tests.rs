use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "grapt-source-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn bound(size: usize) -> Result<MaxDocumentBytes, String> {
    MaxDocumentBytes::try_new(size).map_err(|error| error.to_string())
}
#[test]
fn unchanged_files_skip_content_reads_and_current_bounds_still_apply(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = Scratch::new()?;
    let path = root.0.join("AGENTS.md");
    std::fs::write(&path, "original")?;
    let mut cache = Cache::default();
    let first = cache.load(&path, bound(64)?)?.ok_or("source")?;
    let same = cache
        .load_with(&path, bound(64)?, |_, _| {
            Err("cache hit read the file again".into())
        })?
        .ok_or("source")?;
    assert!(Arc::ptr_eq(&first, &same));
    assert!(
        cache.load(&path, bound(1)?).is_err(),
        "a hit must obey the current byte bound"
    );
    assert!(cache.entries.is_empty());
    Ok(())
}
#[test]
fn changes_deletion_unreadable_replacement_and_recovery_do_not_resurrect_stale_bytes(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = Scratch::new()?;
    let path = root.0.join("config.json");
    let mut cache = Cache::default();
    std::fs::write(&path, "old")?;
    cache.load(&path, bound(64)?)?;
    std::fs::write(&path, "longer")?;
    assert!(cache
        .load_with(&path, bound(64)?, |_, _| Err(
            "injected unreadable source".into()
        ))
        .is_err());
    assert!(cache.entries.is_empty());
    assert_eq!(&*cache.load(&path, bound(64)?)?.ok_or("source")?, b"longer");
    std::fs::remove_file(&path)?;
    assert!(cache.load(&path, bound(64)?)?.is_none());
    std::fs::create_dir(&path)?;
    assert!(cache.load(&path, bound(64)?).is_err());
    std::fs::remove_dir(&path)?;
    std::fs::write(&path, "new")?;
    assert_eq!(&*cache.load(&path, bound(64)?)?.ok_or("source")?, b"new");
    Ok(())
}
#[test]
fn an_in_read_change_is_refused_and_the_next_load_recovers(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = Scratch::new()?;
    let path = root.0.join("AGENTS.md");
    std::fs::write(&path, "old")?;
    let mut cache = Cache::default();
    assert!(cache
        .load_with(&path, bound(64)?, |_, _| {
            std::fs::write(&path, "replacement").map_err(|error| error.to_string())?;
            Ok(b"old".to_vec())
        })
        .is_err());
    assert!(cache.entries.is_empty());
    assert_eq!(
        &*cache.load(&path, bound(64)?)?.ok_or("source")?,
        b"replacement"
    );
    Ok(())
}
#[test]
fn equal_length_changes_with_a_new_mtime_invalidate() -> Result<(), Box<dyn std::error::Error>> {
    let root = Scratch::new()?;
    let path = root.0.join("AGENTS.md");
    std::fs::write(&path, "old")?;
    let mut cache = Cache::default();
    cache.load(&path, bound(64)?)?;
    std::fs::write(&path, "new")?;
    let file = std::fs::File::options().write(true).open(&path)?;
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1234)),
    )?;
    assert_eq!(&*cache.load(&path, bound(64)?)?.ok_or("source")?, b"new");
    Ok(())
}
#[test]
fn retained_paths_and_bytes_are_bounded() -> Result<(), Box<dyn std::error::Error>> {
    let root = Scratch::new()?;
    let mut cache = Cache::default();
    for index in 0..=MAX_FILES {
        let path = root.0.join(index.to_string());
        std::fs::write(&path, "x")?;
        cache.load(&path, bound(64)?)?;
    }
    assert_eq!(cache.entries.len(), MAX_FILES);
    assert_eq!(cache.order.len(), MAX_FILES);
    assert!(!cache.entries.contains_key(&root.0.join("0")));
    assert!(cache.bytes <= MAX_BYTES);
    Ok(())
}

#[test]
fn decoded_documents_obey_new_parse_bounds_and_do_not_keep_invalid_replacements(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = Scratch::new()?;
    let path = root.0.join("grapt.json");
    std::fs::write(&path, r#"{"a":{"b":1}}"#)?;
    let bounds = super::super::catalog_config::bounds()?;
    let first = document(&path, bounds)?.ok_or("document")?;
    assert_eq!(document(&path, bounds)?.ok_or("document")?, first);
    let strict = ConfigBounds::new(
        bounds.document_bytes(),
        bounds.ancestor_depth(),
        bounds.documents(),
        grapt_config::MaxValueDepth::try_new(1)?,
        bounds.document_nodes(),
    );
    assert!(
        document(&path, strict).is_err(),
        "a cached AST cannot bypass tighter decoding bounds"
    );
    std::fs::write(&path, "malformed")?;
    assert!(
        document(&path, bounds).is_err(),
        "an invalid replacement must not return the old AST"
    );
    std::fs::write(&path, r#"{"recovered":true}"#)?;
    assert_ne!(document(&path, bounds)?.ok_or("document")?, first);
    Ok(())
}

#[test]
fn byte_pressure_evicts_old_sources_before_the_file_count_limit(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = Scratch::new()?;
    let mut cache = Cache::default();
    let bytes = vec![b'x'; 65_536];
    for index in 0..=MAX_BYTES / bytes.len() {
        let path = root.0.join(index.to_string());
        std::fs::write(&path, &bytes)?;
        cache.load(&path, bound(bytes.len())?)?;
    }
    assert!(cache.bytes <= MAX_BYTES);
    assert_eq!(cache.entries.len(), MAX_BYTES / bytes.len());
    assert!(!cache.entries.contains_key(&root.0.join("0")));
    Ok(())
}

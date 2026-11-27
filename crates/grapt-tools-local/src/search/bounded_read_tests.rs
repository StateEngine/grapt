use super::*;
use crate::filesystem::capture;

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Result<Self, String> {
        let path =
            std::env::temp_dir().join(format!("grapt-first-use-{}-{label}", std::process::id()));
        std::fs::create_dir(&path)
            .map_err(|error| format!("exclusive scratch creation: {error}"))?;
        Ok(Self(
            std::fs::canonicalize(path).map_err(|error| error.to_string())?,
        ))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

struct MeteredReader {
    remaining: usize,
    captured: usize,
}

impl Read for MeteredReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = buffer.len().min(self.remaining);
        buffer[..count].fill(b'x');
        self.remaining -= count;
        self.captured += count;
        Ok(count)
    }
}

#[test]
fn nt_capture_caps_reader_before_allocation_and_preserves_errors() -> Result<(), String> {
    for cap in [0, 2, 64 * 1024, FILE_EFFECT_MAX_BYTES] {
        let mut reader = MeteredReader {
            remaining: (cap as usize + 1) * 4,
            captured: 0,
        };
        let error = capture(&mut reader, cap).err().ok_or("overflow accepted")?;
        assert!(error.contains("byte bound exceeded"), "{error}");
        assert_eq!(
            reader.captured as u64,
            cap + 1,
            "capture consumed beyond overflow sentinel"
        );
    }
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("reader unavailable"))
        }
    }
    let error = capture(Broken, 16).err().ok_or("unreadable accepted")?;
    assert!(error.contains("unreadable") && !error.contains("bound exceeded"));
    assert_eq!(capture(std::io::Cursor::new(b"exact"), 5)?, b"exact");
    Ok(())
}

#[test]
fn nt_capture_growth_after_stat_and_remaining_ignore_budget_are_bounded() -> Result<(), String> {
    let scratch = Scratch::new("nt-capture-growth")?;
    let path = scratch.0.join("growing");
    std::fs::write(&path, b"small").map_err(|error| error.to_string())?;
    let size = std::fs::metadata(&path)
        .map_err(|error| error.to_string())?
        .len();
    let reader = std::fs::File::open(&path).map_err(|error| error.to_string())?;
    std::fs::write(&path, [b'x'; 128]).map_err(|error| error.to_string())?;
    let error = capture(reader, size)
        .err()
        .ok_or("post-stat growth accepted")?;
    assert!(error.contains("byte bound exceeded"), "{error}");
    std::fs::write(scratch.0.join(".gitignore"), b"abc\n").map_err(|error| error.to_string())?;
    let mut count = 0;
    let mut bytes = 512 * 1024 - 2;
    let error = load_ignore(&scratch.0, &mut count, &mut bytes)
        .err()
        .ok_or("ignore aggregate overflow accepted")?;
    assert!(error.contains("byte bound exceeded"), "{error}");
    assert_eq!(
        bytes,
        512 * 1024 - 2,
        "overflow entered retained ignore budget"
    );
    Ok(())
}

#[test]
fn p7_ignore_file_count_bound_is_enforced() -> Result<(), String> {
    let scratch = Scratch::new("p7-ignore-count")?;
    std::fs::write(scratch.0.join(".ignore"), "bounded\n").map_err(|error| error.to_string())?;
    let mut count = 64;
    let error = load_ignore(&scratch.0, &mut count, &mut 0)
        .err()
        .ok_or("ignore file count bound bypassed")?;
    assert!(error.contains("count/byte bound"), "{error}");
    Ok(())
}

//! Read executable pages during fixture preparation, before timed operations.
use std::{fs, fs::File, io::Read, path::Path};

pub const MAX_EXECUTABLE_BYTES: u64 = 1024 * 1024 * 1024;

pub fn prepare(path: &Path) -> Result<u64, String> {
    let failure = |detail| format!("fixture executable preparation: {detail}");
    let metadata = fs::metadata(path).map_err(|error| failure(error.to_string()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_EXECUTABLE_BYTES {
        return Err(failure(
            "not a nonempty regular file within the 1 GiB bound".to_owned(),
        ));
    }
    let mut file = File::open(path).map_err(|error| failure(error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| failure(error.to_string()))?;
    let length = metadata.len();
    if !metadata.is_file() || length == 0 || length > MAX_EXECUTABLE_BYTES {
        return Err(failure(
            "not a nonempty regular file within the 1 GiB bound".to_owned(),
        ));
    }
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    for _ in 0..=MAX_EXECUTABLE_BYTES / buffer.len() as u64 {
        let count = file
            .read(&mut buffer)
            .map_err(|error| failure(error.to_string()))?;
        if count == 0 {
            return if total == length {
                Ok(total)
            } else {
                Err(failure(
                    "executable length changed during preparation".to_owned(),
                ))
            };
        }
        total += count as u64;
        if total > MAX_EXECUTABLE_BYTES {
            return Err(failure("executable exceeded its read bound".to_owned()));
        }
    }
    Err(failure(
        "executable exceeded its read count bound".to_owned(),
    ))
}

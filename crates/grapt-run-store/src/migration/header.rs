//! The SQLite header a connection would see, read with plain file I/O before
//! SQLite opens the file. A writable SQLite connection replays a hot journal
//! on its first read and checkpoints a WAL when it closes, so a file that is
//! not this store, or was written by a newer build, is refused here and keeps
//! its bytes, its journal and its WAL untouched.
//!
//! In WAL mode the newest committed header can live in the WAL rather than the
//! file, so the WAL's frames are walked as SQLite recovers them: frames count
//! while their salts and running checksum hold, and only up to the last commit.
//!
//! The file is named by path here and again when SQLite opens it. A process
//! running as the same user that renames another file into place between the
//! two can bypass this check; the store cannot close that gap without handles
//! SQLite does not take from safe code, so it is out of scope.
use crate::error::OpenError;
use crate::sqlite::sidecar;
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read};
use std::path::Path;

const HEADER: usize = 100;
const MAGIC: &[u8; 16] = b"SQLite format 3\0";
const USER_VERSION: usize = 60;
const APPLICATION_ID: usize = 68;

const WAL_HEADER: usize = 32;
const FRAME_HEADER: usize = 24;
/// The low bit says whether checksums read words big-endian.
const WAL_MAGIC: u32 = 0x377f_0682;
const WAL_VERSION: u32 = 3_007_000;

/// Refuse a foreign or newer store at `path`, judged by its file header and by
/// the newest header committed to its WAL. A missing or empty file with no
/// committed WAL is a new store. A non-empty file without this store's
/// application id is foreign, even if it is SQLite: a new store gets its id in
/// its first transaction, before it ever has a WAL.
pub(super) fn check(path: &Path, application_id: u32, max_supported: u32) -> Result<(), OpenError> {
    let file = file_header(path)?;
    let wal = committed(&sidecar(path, "-wal"))?;
    if file.is_none() && wal.any {
        // SQLite would delete this WAL; it belongs to no database here.
        return Err(OpenError::Schema(0));
    }
    for header in file.iter().chain(wal.header.iter()) {
        judge(header, application_id, max_supported)?;
    }
    Ok(())
}

fn file_header(path: &Path) -> Result<Option<Vec<u8>>, OpenError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut header = Vec::with_capacity(HEADER);
    file.take(HEADER as u64).read_to_end(&mut header)?;
    Ok((!header.is_empty()).then_some(header))
}

fn judge(header: &[u8], application_id: u32, max_supported: u32) -> Result<(), OpenError> {
    if header.len() < HEADER || !header.starts_with(MAGIC) {
        return Err(OpenError::Schema(0));
    }
    let version = i64::from(i32::from_be_bytes(word(header, USER_VERSION)));
    if u32::from_be_bytes(word(header, APPLICATION_ID)) != application_id {
        return Err(OpenError::Schema(version));
    }
    if version > i64::from(max_supported) {
        return Err(OpenError::FutureSchemaVersion {
            found: version,
            max_supported,
        });
    }
    Ok(())
}

/// What a WAL's committed transactions hold, as SQLite would recover them.
#[derive(Default)]
struct Committed {
    /// Whether any transaction committed at all.
    any: bool,
    /// The first bytes of the newest committed page 1, if one was committed;
    /// otherwise SQLite reads the header from the file itself.
    header: Option<Vec<u8>>,
}

/// Nothing is committed when the WAL is absent or unrecognised.
fn committed(path: &Path) -> Result<Committed, OpenError> {
    let mut wal = match File::open(path) {
        Ok(file) => BufReader::new(file),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Committed::default()),
        Err(error) => return Err(error.into()),
    };
    let mut header = [0; WAL_HEADER];
    if !fill(&mut wal, &mut header)? {
        return Ok(Committed::default());
    }
    let magic = u32::from_be_bytes(word(&header, 0));
    let page = u32::from_be_bytes(word(&header, 8)) as usize;
    if magic & !1 != WAL_MAGIC
        || u32::from_be_bytes(word(&header, 4)) != WAL_VERSION
        || !(512..=65_536).contains(&page)
        || !page.is_power_of_two()
    {
        return Ok(Committed::default());
    }
    let big_endian = magic & 1 == 1;
    let mut sum = checksum(big_endian, (0, 0), &header[..24]);
    if sum != stored(&header, 24) {
        return Ok(Committed::default());
    }
    let mut frame = vec![0; FRAME_HEADER + page];
    let (mut pending, mut committed) = (None, Committed::default());
    while fill(&mut wal, &mut frame)? {
        if frame[8..16] != header[16..24] {
            break;
        }
        sum = checksum(big_endian, sum, &frame[..8]);
        sum = checksum(big_endian, sum, &frame[FRAME_HEADER..]);
        if sum != stored(&frame, 16) {
            break;
        }
        if u32::from_be_bytes(word(&frame, 0)) == 1 {
            pending = Some(frame[FRAME_HEADER..FRAME_HEADER + HEADER].to_vec());
        }
        if u32::from_be_bytes(word(&frame, 4)) != 0 {
            committed.any = true;
            if let Some(page_one) = pending.take() {
                committed.header = Some(page_one);
            }
        }
    }
    Ok(committed)
}

/// Fill `buffer` entirely, or report a short tail as the end of the WAL.
fn fill(reader: &mut impl Read, buffer: &mut [u8]) -> Result<bool, OpenError> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// SQLite's WAL checksum, continued from `sum` over 8-byte steps of `data`.
fn checksum(big_endian: bool, sum: (u32, u32), data: &[u8]) -> (u32, u32) {
    let read = |at: usize| match big_endian {
        true => u32::from_be_bytes(word(data, at)),
        false => u32::from_le_bytes(word(data, at)),
    };
    let (mut first, mut second) = sum;
    for at in (0..data.len()).step_by(8) {
        first = first.wrapping_add(read(at)).wrapping_add(second);
        second = second.wrapping_add(read(at + 4)).wrapping_add(first);
    }
    (first, second)
}

fn stored(bytes: &[u8], at: usize) -> (u32, u32) {
    (
        u32::from_be_bytes(word(bytes, at)),
        u32::from_be_bytes(word(bytes, at + 4)),
    )
}

fn word(bytes: &[u8], at: usize) -> [u8; 4] {
    [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
}

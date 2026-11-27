//! Bounded decode of uncompressed ustar archives.
//!
//! The only archive format the boundary admits. The stream is walked
//! in-memory under the declared bounds; nothing is extracted to disk, so the
//! "unpacked tree" the layout bounds is the decoded value itself. Every
//! structural inconsistency is a named refusal, never a silent truncation.

use std::collections::BTreeSet;

use crate::refusal::{Refusal, RefusalReason};
use crate::value::EntryName;

/// The archive byte bound: 8 MiB. The single bound the boundary declares:
/// an archive over the bound cannot be decoded, and a binary is never
/// larger than the archive that carries it.
pub const ARCHIVE_MAX_BYTES: usize = 8 * 1024 * 1024;
/// The binary byte bound: the archive bound, named for the binary read
/// paths. A decoded binary over the bound is a defect of the pin, refused
/// before anything is staged.
pub const BINARY_MAX_BYTES: usize = ARCHIVE_MAX_BYTES;
/// The entry count bound: 16.
pub const TAR_MAX_ENTRIES: usize = 16;
/// The path depth bound: 4.
pub const TAR_MAX_DEPTH: usize = 4;
/// The entry name bound: 256 bytes.
pub const TAR_MAX_NAME_BYTES: usize = 256;

const BLOCK: usize = 512;

/// One decoded entry: a regular file, or a directory (no bytes).
#[derive(Debug)]
pub struct DecodedEntry {
    /// The normalized entry path, '/'-separated, relative.
    pub name: String,
    /// The entry bytes; empty for a directory.
    pub bytes: Vec<u8>,
    directory: bool,
}

/// The decoded archive: at most 16 entries, every path checked against the
/// bounds, duplicates refused.
#[derive(Debug)]
pub struct DecodedArchive {
    entries: Vec<DecodedEntry>,
}

impl DecodedArchive {
    /// The bytes of the REGULAR entry named exactly `name`; `None` when
    /// the archive does not contain it (the caller's missing-binary fact).
    /// Directory entries are not binaries and are never returned.
    pub fn entry(&self, name: &EntryName) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|entry| !entry.directory && entry.name == name.as_str())
            .map(|entry| entry.bytes.as_slice())
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

/// Decode an uncompressed ustar stream under the declared bounds.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn decode_tar(bytes: &[u8]) -> Result<DecodedArchive, Refusal> {
    let truncated = |what: &'static str| Refusal::new(RefusalReason::ArchiveTruncated, what);
    let malformed = |what: &'static str| Refusal::new(RefusalReason::ArchiveMalformed, what);
    let unsafe_entry = |what: &'static str| Refusal::new(RefusalReason::ArchiveUnsafeEntry, what);
    if bytes.len() > ARCHIVE_MAX_BYTES {
        return Err(malformed("the archive exceeds the 8 MiB bound"));
    }
    if bytes.is_empty() {
        return Err(truncated("the archive is empty: the terminator is missing"));
    }
    if !bytes.len().is_multiple_of(BLOCK) {
        return Err(truncated(
            "the archive is not a whole number of 512-byte blocks",
        ));
    }
    let blocks = bytes.len() / BLOCK;
    let mut entries = Vec::new();
    let mut names = BTreeSet::new();
    let mut block = 0usize;
    let mut terminated = false;
    while block < blocks {
        let header = &bytes[block * BLOCK..(block + 1) * BLOCK];
        if header.iter().all(|b| *b == 0) {
            // The terminator is exactly the final two zero blocks.
            if block + 1 == blocks {
                return Err(truncated(
                    "the stream ends in a single zero block: the terminator is missing",
                ));
            }
            if block + 2 != blocks {
                return Err(truncated("data follows the terminator"));
            }
            terminated = true;
            break;
        }
        let name = c_string(header, 0, 100)
            .ok_or_else(|| malformed("the entry name is not NUL-terminated"))?;
        let prefix = c_string(header, 345, 155).unwrap_or_default();
        let size =
            octal_field(header, 124, 8).ok_or_else(|| malformed("the entry size is not octal"))?;
        let size = usize::try_from(size)
            .map_err(|_| malformed("the entry size overflows the address space"))?;
        let declared_checksum = octal_field(header, 140, 8)
            .ok_or_else(|| malformed("the header checksum is not octal"))?;
        let mut sum = 0u64;
        for (index, byte) in header.iter().enumerate() {
            // The declared checksum is spelled with the field read as spaces.
            sum += if (140..148).contains(&index) {
                0x20
            } else {
                u64::from(*byte)
            };
        }
        if declared_checksum != sum {
            return Err(malformed("the header checksum does not verify"));
        }
        if &header[257..263] != b"ustar\0" {
            return Err(malformed("the header is not ustar"));
        }
        if &header[263..265] != b"00" {
            return Err(malformed("the ustar version is not 00"));
        }
        let typeflag = header[148];
        let full_name = if prefix.is_empty() {
            name.to_owned()
        } else if name.is_empty() {
            return Err(malformed("an entry carries a prefix but no name"));
        } else {
            format!("{prefix}/{name}")
        };
        let validated = validate_name(&full_name).ok_or_else(|| {
            unsafe_entry("the entry path is absolute, carries a dot segment, or exceeds a bound")
        })?;
        if !names.insert(validated.clone()) {
            return Err(unsafe_entry("the archive carries a duplicate entry path"));
        }
        if entries.len() >= TAR_MAX_ENTRIES {
            return Err(unsafe_entry("the archive exceeds the 16-entry bound"));
        }
        let data_blocks = size.div_ceil(BLOCK);
        if block + 1 + data_blocks > blocks {
            return Err(truncated("the entry size overruns the stream"));
        }
        let data_start = (block + 1) * BLOCK;
        let data = &bytes[data_start..data_start + size];
        let block_end = (block + 1 + data_blocks) * BLOCK;
        if bytes[data_start + size..block_end].iter().any(|b| *b != 0) {
            return Err(truncated("the entry padding is not zero"));
        }
        match typeflag {
            b'0' | b'\0' => entries.push(DecodedEntry {
                name: validated,
                bytes: data.to_vec(),
                directory: false,
            }),
            b'5' => {
                if size != 0 {
                    return Err(malformed("a directory entry declares a non-zero size"));
                }
                entries.push(DecodedEntry {
                    name: validated,
                    bytes: Vec::new(),
                    directory: true,
                });
            }
            _ => {
                return Err(unsafe_entry(
                    "the archive carries a symlink, hardlink, special or unknown entry",
                ))
            }
        }
        block += 1 + data_blocks;
    }
    if !terminated {
        return Err(truncated(
            "the stream ends where its entries end: the terminator is missing",
        ));
    }
    Ok(DecodedArchive { entries })
}

/// The admitted entry path shape: relative, '/'-separated, no dot segments,
/// no backslash, at most 4 segments deep, at most 256 bytes.
fn validate_name(name: &str) -> Option<String> {
    if name.is_empty() || name.len() > TAR_MAX_NAME_BYTES {
        return None;
    }
    if name.contains('\\') {
        return None;
    }
    let mut depth = 0usize;
    for segment in name.split('/') {
        depth += 1;
        if depth > TAR_MAX_DEPTH {
            return None;
        }
        if segment.is_empty() || segment == "." || segment == ".." {
            return None;
        }
    }
    Some(name.to_owned())
}

/// A NUL-terminated ASCII field; an all-NUL field decodes to the empty
/// string, a field without its terminator decodes to nothing.
fn c_string(block: &[u8], start: usize, len: usize) -> Option<&str> {
    let field = &block[start..start + len];
    let end = field.iter().position(|b| *b == 0)?;
    std::str::from_utf8(&field[..end]).ok()
}

/// An octal ustar field, space- or NUL-padded.
fn octal_field(block: &[u8], start: usize, len: usize) -> Option<u64> {
    let field = &block[start..start + len];
    let text = std::str::from_utf8(field).ok()?;
    let trimmed = text.trim_matches(|c| c == ' ' || c == '\0').trim_end();
    if trimmed.is_empty() {
        return None;
    }
    u64::from_str_radix(trimmed, 8).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write an octal ustar field: 7 digits + NUL.
    fn write_octal(field: &mut [u8], value: u64) {
        let text = format!("{value:07o}");
        field[..text.len()].copy_from_slice(text.as_bytes());
        field[text.len()] = 0;
    }

    fn header_block(name: &str, prefix: &str, size: u64, typeflag: u8) -> Vec<u8> {
        let mut block = [0u8; 512];
        block[..name.len()].copy_from_slice(name.as_bytes());
        if !prefix.is_empty() {
            block[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        }
        write_octal(&mut block[124..132], size);
        block[148] = typeflag;
        block[257..263].copy_from_slice(b"ustar\0");
        block[263..265].copy_from_slice(b"00");
        let mut sum = 0u64;
        for (index, byte) in block.iter().enumerate() {
            sum += if (140..148).contains(&index) {
                0x20
            } else {
                u64::from(*byte)
            };
        }
        write_octal(&mut block[140..148], sum);
        block.to_vec()
    }

    fn data_blocks(bytes: &[u8]) -> Vec<u8> {
        let padded = bytes.len().div_ceil(BLOCK) * BLOCK;
        let mut out = bytes.to_vec();
        out.resize(padded, 0);
        out
    }

    fn archive(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in parts {
            out.extend_from_slice(part);
        }
        out.extend_from_slice(&[0u8; BLOCK * 2]);
        out
    }

    fn file_entry(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut out = header_block(name, "", u64::try_from(bytes.len()).unwrap_or(u64::MAX), 0);
        out.extend(data_blocks(bytes));
        out
    }

    fn refusal_of(bytes: &[u8]) -> Refusal {
        match decode_tar(bytes) {
            Ok(_) => panic!("this archive must be refused"),
            Err(refusal) => refusal,
        }
    }

    #[test]
    fn a_valid_stream_decodes_its_entries() {
        let stream = archive(&[
            file_entry("rg.exe", b"binary-bytes"),
            file_entry("dir/inner", b"inner-bytes"),
        ]);
        let decoded =
            decode_tar(&stream).unwrap_or_else(|r| panic!("valid archive must decode: {r}"));
        assert_eq!(decoded.entry_count(), 2);
        let rg = EntryName::new("rg.exe").unwrap_or_else(|| panic!("fixture"));
        assert_eq!(decoded.entry(&rg), Some(b"binary-bytes".as_slice()));
        let inner = EntryName::new("dir/inner").unwrap_or_else(|| panic!("fixture"));
        assert_eq!(decoded.entry(&inner), Some(b"inner-bytes".as_slice()));
        let missing = EntryName::new("absent").unwrap_or_else(|| panic!("fixture"));
        assert!(decoded.entry(&missing).is_none());
    }

    #[test]
    fn a_directory_is_decoded_but_never_returned_as_a_binary() {
        let mut stream = header_block("sub", "", 0, b'5');
        stream.extend(file_entry("sub/tool.bin", b"bin"));
        let terminator = [0u8; BLOCK * 2];
        stream.extend_from_slice(&terminator);
        let decoded =
            decode_tar(&stream).unwrap_or_else(|r| panic!("valid archive must decode: {r}"));
        assert_eq!(decoded.entry_count(), 2);
        let sub = EntryName::new("sub").unwrap_or_else(|| panic!("fixture"));
        assert!(decoded.entry(&sub).is_none(), "a directory is not a binary");
        let tool = EntryName::new("sub/tool.bin").unwrap_or_else(|| panic!("fixture"));
        assert_eq!(decoded.entry(&tool), Some(b"bin".as_slice()));
    }

    #[test]
    fn a_prefix_field_completes_the_entry_path() {
        let mut out = header_block("tool.bin", "sub", 3, 0);
        out.extend(data_blocks(b"bin"));
        let terminator = [0u8; BLOCK * 2];
        out.extend_from_slice(&terminator);
        let decoded = decode_tar(&out).unwrap_or_else(|r| panic!("valid archive must decode: {r}"));
        let tool = EntryName::new("sub/tool.bin").unwrap_or_else(|| panic!("fixture"));
        assert_eq!(decoded.entry(&tool), Some(b"bin".as_slice()));
    }

    #[test]
    fn a_symlink_or_hardlink_refuses_unsafe() {
        for typeflag in *b"12" {
            let mut out = header_block("link", "", 0, typeflag);
            let terminator = [0u8; BLOCK * 2];
            out.extend_from_slice(&terminator);
            assert_eq!(refusal_of(&out).reason, RefusalReason::ArchiveUnsafeEntry);
        }
    }

    #[test]
    fn an_absolute_or_dot_segment_path_refuses_unsafe() {
        for name in [
            "/absolute",
            "a/../b",
            "a/./b",
            "a\\b",
            "a/b/c/d/e",
            "trailing/",
        ] {
            let out = archive(&[file_entry(name, b"x")]);
            assert_eq!(
                refusal_of(&out).reason,
                RefusalReason::ArchiveUnsafeEntry,
                "{name} must refuse unsafe-entry"
            );
        }
    }

    #[test]
    fn a_duplicate_entry_path_refuses_unsafe() {
        let out = archive(&[file_entry("same", b"one"), file_entry("same", b"two")]);
        assert_eq!(refusal_of(&out).reason, RefusalReason::ArchiveUnsafeEntry);
    }

    #[test]
    fn a_seventeenth_entry_refuses_unsafe() {
        let mut parts = Vec::new();
        for index in 0..17usize {
            parts.push(file_entry(&format!("e{index}"), b"x"));
        }
        assert_eq!(
            refusal_of(&archive(&parts)).reason,
            RefusalReason::ArchiveUnsafeEntry
        );
    }

    #[test]
    fn a_bad_checksum_refuses_malformed() {
        let mut out = file_entry("rg", b"bin");
        out[100] ^= 0xff; // corrupt one header byte: the checksum no longer verifies
        let rest: Vec<u8> = archive(&[out]);
        assert_eq!(refusal_of(&rest).reason, RefusalReason::ArchiveMalformed);
    }

    #[test]
    fn an_overbound_archive_refuses_malformed() {
        // The corpus cannot reach this row: the shipped source adapter
        // refuses over-bound fetches first. The in-memory decoder is the
        // law, and its bound is witnessed here.
        let overbound = vec![0u8; ARCHIVE_MAX_BYTES + 1];
        assert_eq!(
            refusal_of(&overbound).reason,
            RefusalReason::ArchiveMalformed
        );
    }

    #[test]
    fn a_truncated_stream_refuses_truncated() {
        let full = archive(&[file_entry("rg", b"binary-bytes")]);
        let cut = &full[..full.len() - BLOCK * 3]; // the terminator and part of the data
        assert_eq!(refusal_of(cut).reason, RefusalReason::ArchiveTruncated);
        assert_eq!(refusal_of(b"").reason, RefusalReason::ArchiveTruncated);
    }

    #[test]
    fn a_nonzero_padding_or_missing_terminator_refuses_truncated() {
        let mut out = header_block("rg", "", 1, 0);
        let mut data = [0u8; BLOCK];
        data[0] = b'x';
        data[BLOCK - 1] = 0xff; // non-zero padding byte
        out.extend(data.to_vec());
        out.extend([0u8; BLOCK * 2]);
        assert_eq!(refusal_of(&out).reason, RefusalReason::ArchiveTruncated);

        let mut single = header_block("rg", "", 0, 0);
        single.extend([0u8; BLOCK]); // exactly one zero block: no terminator
        assert_eq!(refusal_of(&single).reason, RefusalReason::ArchiveTruncated);
    }

    #[test]
    fn a_size_overrun_refuses_truncated() {
        let mut out = header_block("rg", "", 1024, 0); // declares 1024 bytes, carries none
        let terminator = [0u8; BLOCK * 2];
        out.extend_from_slice(&terminator);
        assert_eq!(refusal_of(&out).reason, RefusalReason::ArchiveTruncated);
    }

    #[test]
    fn a_bad_magic_refuses_malformed() {
        let mut bad_magic = file_entry("rg", b"bin");
        bad_magic[257..263].copy_from_slice(b"gustar");
        let rest: Vec<u8> = archive(&[bad_magic]);
        assert_eq!(refusal_of(&rest).reason, RefusalReason::ArchiveMalformed);
    }
}

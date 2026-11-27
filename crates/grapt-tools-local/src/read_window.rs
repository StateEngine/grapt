//! Streaming, bounded numbered line windows with captured-byte patch identity.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// Lines in one page when the caller names no limit, and the most it may ask.
pub const READ_LINES: u64 = 2_000;
/// A page ends once its rendered lines reach this many bytes.
const PAGE_BYTES: usize = 50 * 1_024;
/// A line longer than this many characters is cut.
const LINE_CHARS: usize = 2_000;
/// Bytes of a line kept for display; the rest is read past, never held.
const LINE_KEEP_BYTES: usize = LINE_CHARS * 4;
/// A NUL in this many leading bytes marks the file binary.
const BINARY_PROBE: usize = 8 * 1_024;
/// The most bytes one read may consume, skipped lines and cut tails
/// included, as discovery bounds a search.
const SCAN_BYTES: u64 = 16 * 1_024 * 1_024;

/// One page of `path`, starting at the 1-based `offset` line.
///
/// The file is opened ONCE and captured: its first `sha_bound + 1` bytes are
/// buffered and give the binary probe and the start of the page. A file that
/// fits is hashed and paged from that buffer ALONE, so the sha always names
/// the lines shown; only a larger file, which reports no sha, continues from
/// the same handle. Every byte consumed counts against [`SCAN_BYTES`].
pub fn page(
    path: &Path,
    display: &str,
    offset: u64,
    limit: u64,
    sha_bound: u64,
) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("read open failed: {error}"))?;
    let capture = Capture::of(&file, display, sha_bound)?;
    capture.page(file, display, offset, limit)
}

/// What one read captured of a file before paging it.
pub(super) struct Capture {
    head: Vec<u8>,
    sha: Option<String>,
}

impl Capture {
    pub(super) fn of(file: &std::fs::File, display: &str, sha_bound: u64) -> Result<Self, String> {
        let mut head = Vec::new();
        file.take(sha_bound + 1)
            .read_to_end(&mut head)
            .map_err(|error| format!("read failed: {error}"))?;
        if head[..head.len().min(BINARY_PROBE)].contains(&0) {
            return Err(format!(
                "read refuses {display}: a NUL byte in its first 8 KB marks it binary"
            ));
        }
        let sha = (head.len() as u64 <= sha_bound).then(|| {
            ring::digest::digest(&ring::digest::SHA256, &head)
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        });
        Ok(Self { head, sha })
    }

    /// The page, from the captured bytes alone when they were hashed.
    pub(super) fn page(
        self,
        rest: std::fs::File,
        display: &str,
        offset: u64,
        limit: u64,
    ) -> Result<String, String> {
        let Self { head, sha } = self;
        let rest: Box<dyn Read> = if sha.is_some() {
            Box::new(std::io::empty())
        } else {
            Box::new(rest)
        };
        let mut reader = BufReader::new(std::io::Cursor::new(head).chain(rest));
        let mut scan = Scan::default();
        let mut rendered = String::new();
        let mut number = 0u64;
        let mut shown = 0u64;
        let mut next = None;
        loop {
            let shown_line = number + 1 >= offset && shown < limit;
            let Some((kept, cut, valid)) = next_line(&mut reader, &mut scan, shown_line)? else {
                break;
            };
            number += 1;
            if scan.exhausted {
                if shown == 0 {
                    return Err(format!(
                        "read scans at most {} MiB per call and reached line {number} of \
                         {display} before offset {offset}; lines past that point are beyond \
                         read (and grep reads files of at most 1 MiB), so view them with the \
                         shell, e.g. sed -n '{offset},{}p'",
                        SCAN_BYTES / (1_024 * 1_024),
                        offset.saturating_add(limit - 1)
                    ));
                }
                next = Some(number);
                break;
            }
            if number < offset {
                continue;
            }
            if shown == limit {
                next = Some(number);
                break;
            }
            let line = if valid {
                render(&kept, cut, number)
            } else {
                Err(format!("read content is not UTF-8 at line {number}"))
            };
            let line = match line {
                Ok(line) => line,
                // A later line that cannot show ends the page; the next page names it.
                Err(_) if shown > 0 => {
                    next = Some(number);
                    break;
                }
                Err(error) => return Err(error),
            };
            if shown > 0 && rendered.len() + line.len() > PAGE_BYTES {
                next = Some(number);
                break;
            }
            rendered.push_str(&line);
            shown += 1;
        }
        if shown == 0 && offset > 1 {
            return Err(format!(
                "read offset {offset} is past the end of {display} ({number} lines)"
            ));
        }
        if let Some(next) = next {
            rendered.push_str(&format!(
                "[Output truncated. Continue reading with offset: {next}]\n"
            ));
        }
        if let Some(sha) = sha {
            rendered.push_str(&format!("[sha256 for patch: {sha}]\n"));
        }
        Ok(rendered)
    }
}

/// The bytes one call has consumed, against [`SCAN_BYTES`].
#[derive(Default)]
struct Scan {
    bytes: u64,
    exhausted: bool,
}

/// The next line's bytes without its line ending, keeping at most
/// [`LINE_KEEP_BYTES`]; whether more was read past; and, when `validate`,
/// whether the WHOLE line is UTF-8 (checked as it streams, so a cut tail is
/// checked too). `None` at end of file. Stops early, marking the scan
/// exhausted, once [`SCAN_BYTES`] have been consumed.
fn next_line(
    reader: &mut impl BufRead,
    scan: &mut Scan,
    validate: bool,
) -> Result<Option<(Vec<u8>, bool, bool)>, String> {
    let mut kept = Vec::new();
    let mut cut = false;
    let mut any = false;
    let mut valid = true;
    let mut carry: Vec<u8> = Vec::new();
    loop {
        if scan.bytes >= SCAN_BYTES {
            scan.exhausted = true;
            break;
        }
        let buffer = reader
            .fill_buf()
            .map_err(|error| format!("read failed: {error}"))?;
        if buffer.is_empty() {
            break;
        }
        any = true;
        let room = usize::try_from(SCAN_BYTES - scan.bytes).unwrap_or(usize::MAX);
        let buffer = &buffer[..buffer.len().min(room)];
        let (chunk, ended) = match buffer.iter().position(|byte| *byte == b'\n') {
            Some(end) => (&buffer[..end], Some(end + 1)),
            None => (buffer, None),
        };
        if validate && valid {
            carry.extend_from_slice(chunk);
            match std::str::from_utf8(&carry) {
                Ok(_) => carry.clear(),
                Err(error) if error.error_len().is_none() => {
                    carry.drain(..error.valid_up_to());
                }
                Err(_) => valid = false,
            }
        }
        let keep = LINE_KEEP_BYTES.saturating_sub(kept.len());
        cut |= chunk.len() > keep;
        kept.extend_from_slice(&chunk[..chunk.len().min(keep)]);
        let consumed = ended.unwrap_or(buffer.len());
        reader.consume(consumed);
        scan.bytes += consumed as u64;
        if ended.is_some() {
            break;
        }
    }
    // An exhausted scan is never mistaken for the end of the file.
    if !any && !scan.exhausted {
        return Ok(None);
    }
    if !carry.is_empty() && !scan.exhausted {
        valid = false;
    }
    if !cut && kept.last() == Some(&b'\r') {
        kept.pop();
    }
    Ok(Some((kept, cut, valid)))
}

/// `N: text`, cut at [`LINE_CHARS`] characters.
fn render(kept: &[u8], cut: bool, number: u64) -> Result<String, String> {
    let text = match std::str::from_utf8(kept) {
        Ok(text) => text,
        // Only a cut prefix may end inside a character.
        Err(error) if error.error_len().is_none() && cut => {
            std::str::from_utf8(&kept[..error.valid_up_to()])
                .map_err(|_| "read UTF-8 boundary validation failed")?
        }
        Err(_) => return Err(format!("read content is not UTF-8 at line {number}")),
    };
    let mut characters = text.char_indices();
    Ok(match characters.nth(LINE_CHARS) {
        Some((cut, _)) => format!(
            "{number}: {}... (line truncated to {LINE_CHARS} chars)\n",
            &text[..cut]
        ),
        None if cut => {
            format!("{number}: {text}... (line truncated to {LINE_CHARS} chars)\n")
        }
        None => format!("{number}: {text}\n"),
    })
}

#[cfg(test)]
mod tests;

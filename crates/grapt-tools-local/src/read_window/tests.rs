//! The read line window: numbering, paging, long lines, binary and size.

use super::*;

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("grapt-read-window-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap_or_else(|error| panic!("scratch: {error}"));
        Self(path)
    }

    fn file(&self, bytes: &[u8]) -> std::path::PathBuf {
        let path = self.0.join("file.txt");
        std::fs::write(&path, bytes).unwrap_or_else(|error| panic!("write: {error}"));
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SHA_BOUND: u64 = 1_024 * 1_024;

fn read(path: &Path, offset: u64, limit: u64) -> String {
    page(path, "file.txt", offset, limit, SHA_BOUND).unwrap_or_else(|error| panic!("{error}"))
}

fn sha(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn lines_are_numbered_from_the_offset_and_crlf_shows_no_carriage_return() {
    let scratch = Scratch::new("numbered");
    let bytes = b"alpha\r\nbeta\r\ngamma";
    let path = scratch.file(bytes);
    assert_eq!(
        read(&path, 1, READ_LINES),
        format!(
            "1: alpha\n2: beta\n3: gamma\n[sha256 for patch: {}]\n",
            sha(bytes)
        )
    );
    assert_eq!(
        read(&path, 2, 1),
        format!(
            "2: beta\n[Output truncated. Continue reading with offset: 3]\n[sha256 for patch: {}]\n",
            sha(bytes)
        )
    );
}

#[test]
fn a_page_ends_near_fifty_kilobytes_and_names_where_to_continue() {
    let scratch = Scratch::new("page");
    let line = "x".repeat(1_000);
    let text: String = (0..200).map(|_| format!("{line}\n")).collect();
    let path = scratch.file(text.as_bytes());
    let first = read(&path, 1, READ_LINES);
    let shown = first.lines().filter(|line| line.contains(": x")).count();
    assert!(
        shown < 200 && first.len() <= PAGE_BYTES + 200,
        "{shown} lines"
    );
    assert!(first.contains(&format!("Continue reading with offset: {}]", shown + 1)));
}

#[test]
fn a_long_line_is_cut_with_a_marker_and_never_held_whole() {
    let scratch = Scratch::new("long");
    let long = "é".repeat(LINE_CHARS + 10);
    let exact = "a".repeat(LINE_KEEP_BYTES);
    let path = scratch.file(format!("{long}\n{exact}\nend\n").as_bytes());
    let output = read(&path, 1, READ_LINES);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(
        lines[0],
        format!(
            "1: {}... (line truncated to {LINE_CHARS} chars)",
            "é".repeat(LINE_CHARS)
        )
    );
    assert!(lines[1].starts_with("2: aaa") && lines[1].ends_with("chars)"));
    assert_eq!(lines[2], "3: end");
}

#[test]
fn a_file_over_one_mebibyte_pages_to_its_last_line_without_a_sha() {
    let scratch = Scratch::new("large");
    let text: String = (1..=120_000)
        .map(|number| format!("line {number}\n"))
        .collect();
    assert!(text.len() as u64 > SHA_BOUND);
    let path = scratch.file(text.as_bytes());
    let mut offset = 1;
    let mut last = String::new();
    for _ in 0..100 {
        let output = read(&path, offset, READ_LINES);
        assert!(
            !output.contains("sha256"),
            "a file over 1 MiB has no patch sha"
        );
        last = output
            .lines()
            .rfind(|line| !line.starts_with('['))
            .unwrap_or_default()
            .to_owned();
        match output
            .lines()
            .find_map(|line| line.strip_prefix("[Output truncated. Continue reading with offset: "))
        {
            Some(next) => {
                offset = next
                    .trim_end_matches(']')
                    .parse()
                    .unwrap_or_else(|_| panic!("{next}"))
            }
            None => break,
        }
    }
    assert_eq!(last, "120000: line 120000");
}

#[test]
fn every_page_of_a_small_file_carries_the_same_sha() {
    let scratch = Scratch::new("sha");
    let text: String = (1..=5).map(|number| format!("{number}\n")).collect();
    let path = scratch.file(text.as_bytes());
    let expected = format!("[sha256 for patch: {}]", sha(text.as_bytes()));
    for offset in 1..=5 {
        assert!(read(&path, offset, 1).contains(&expected), "page {offset}");
    }
}

#[test]
fn a_nul_in_the_first_eight_kilobytes_is_binary_and_later_bad_text_names_its_line() {
    let scratch = Scratch::new("binary");
    let path = scratch.file(b"text\n\0more\n");
    let refused = page(&path, "file.txt", 1, READ_LINES, SHA_BOUND)
        .err()
        .unwrap_or_default();
    assert!(refused.contains("binary"), "{refused}");
    let mut late = "ok\n".repeat(3_000).into_bytes();
    late.extend_from_slice(b"\0nul is text here\n\xff\xfe\n");
    let path = scratch.file(&late);
    // The page stops before the bad line; reading from it names it.
    let shown = read(&path, 3_001, READ_LINES);
    assert!(
        shown.starts_with(
            "3001: \0nul is text here\n[Output truncated. Continue reading with offset: 3002]"
        ),
        "{shown}"
    );
    let refused = page(&path, "file.txt", 3_002, READ_LINES, SHA_BOUND)
        .err()
        .unwrap_or_default();
    assert!(refused.contains("not UTF-8 at line 3002"), "{refused}");
}

#[test]
fn an_offset_past_the_end_names_the_line_count() {
    let scratch = Scratch::new("past");
    let path = scratch.file(b"one\ntwo\n");
    let refused = page(&path, "file.txt", 9, READ_LINES, SHA_BOUND)
        .err()
        .unwrap_or_default();
    assert!(
        refused.contains("past the end of file.txt (2 lines)"),
        "{refused}"
    );
}

#[test]
fn one_read_scans_at_most_sixteen_mebibytes() {
    let scratch = Scratch::new("scan");
    let line = format!("{}\n", "s".repeat(99));
    let path = scratch.file(line.repeat(180_000).as_bytes());
    let refused = page(&path, "file.txt", 179_000, READ_LINES, SHA_BOUND)
        .err()
        .unwrap_or_default();
    assert!(
        refused.contains("scans at most 16 MiB per call")
            && refused.contains("sed -n '179000,180999p'"),
        "{refused}"
    );
    let first = read(&path, 1, 3);
    assert!(first.starts_with("1: sss") && first.contains("Continue reading with offset: 4]"));
}

#[test]
fn invalid_utf8_in_a_cut_tail_names_its_line_but_a_later_unshown_line_does_not_fail() {
    let scratch = Scratch::new("tail");
    let mut bytes = "a".repeat(9_000).into_bytes();
    bytes.extend_from_slice(b"\xff tail\nnext\n");
    let path = scratch.file(&bytes);
    let refused = page(&path, "file.txt", 1, READ_LINES, SHA_BOUND)
        .err()
        .unwrap_or_default();
    assert!(refused.contains("not UTF-8 at line 1"), "{refused}");
    let path = scratch.file(b"ok\n\xff\n");
    let shown = read(&path, 1, 1);
    assert!(
        shown.starts_with("1: ok\n[Output truncated. Continue reading with offset: 2]"),
        "{shown}"
    );
}

#[test]
fn a_hashed_page_shows_only_the_captured_bytes_even_if_the_file_grows() {
    use std::io::Write as _;
    let scratch = Scratch::new("append");
    let path = scratch.file(b"one\ntwo\n");
    let file = std::fs::File::open(&path).unwrap_or_else(|error| panic!("open: {error}"));
    let capture =
        Capture::of(&file, "file.txt", SHA_BOUND).unwrap_or_else(|error| panic!("{error}"));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .and_then(|mut appender| appender.write_all(b"three\n"))
        .unwrap_or_else(|error| panic!("append: {error}"));
    let shown = capture
        .page(file, "file.txt", 1, READ_LINES)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        shown,
        format!(
            "1: one\n2: two\n[sha256 for patch: {}]\n",
            sha(b"one\ntwo\n")
        ),
        "the page showed bytes its sha does not name"
    );
}

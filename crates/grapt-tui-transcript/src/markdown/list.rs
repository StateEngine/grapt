//! Retain complete flat-list items across streaming updates.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Container changes and whitespace-sensitive continuations use the full parser.
#[derive(Debug)]
pub(super) struct OpenList {
    marker: u8,
    end: usize,
}

impl OpenList {
    pub(super) fn detect(source: &str, end: usize) -> Option<Self> {
        let marker = *source.as_bytes().first()?;
        let list = Self { marker, end };
        (matches!(marker, b'-' | b'*' | b'+') && list.flat(source)).then_some(list)
    }

    pub(super) fn append<'a>(&mut self, source: &'a str) -> Option<&'a str> {
        let delta = source.get(self.end..)?;
        if !self.flat(delta) {
            return None;
        }
        self.end = source.len();
        Some(delta)
    }

    pub(super) fn pending_blank(&self, source: &str) -> bool {
        source
            .get(self.end..)
            .is_some_and(|delta| !delta.is_empty() && delta.bytes().all(|byte| byte == b'\n'))
    }

    pub(super) fn following_paragraph(&self, source: &str) -> Option<usize> {
        let delta = source.get(self.end..)?;
        let tail = delta.trim_start_matches('\n');
        // A blank alone can still make the next item part of a loose list.
        // An unindented paragraph after it proves the prior list has ended.
        (tail.len() < delta.len() && tail.chars().next().is_some_and(char::is_alphabetic))
            .then_some(source.len() - tail.len())
    }

    fn flat(&self, source: &str) -> bool {
        source.ends_with('\n')
            && source.split_terminator('\n').all(|line| {
                if line.as_bytes().get(..2) != Some(&[self.marker, b' ']) {
                    return false;
                }
                let text = &line[2..];
                text.chars().next().is_some_and(char::is_alphabetic)
                    && !text.chars().last().is_some_and(char::is_whitespace)
                    && !text.contains('<')
            })
    }
}

/// Bound cold parsing to a short prefix, small batches of tight list items, and a
/// short trailing paragraph. Other shapes retain the normal full-parser path.
pub(super) fn cold_run(source: &str) -> Option<(usize, usize)> {
    if source.chars().any(|ch| {
        matches!(ch, '[' | ']' | '<' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            || (ch.is_control() && ch != '\n')
    }) {
        return None;
    }
    let mut offset = 0;
    let mut boundary = true;
    let mut lines = source.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        if offset > 4096 {
            return None;
        }
        if boundary {
            if let Some(list) = OpenList::detect(line, 0) {
                let start = offset;
                offset += line.len();
                let mut count = 1;
                while let Some(next) = lines.peek() {
                    if !list.flat(next) {
                        break;
                    }
                    offset += next.len();
                    count += 1;
                    lines.next();
                }
                let suffix = &source[offset..];
                let tail = suffix.trim_matches('\n');
                if count >= 32
                    && suffix.len() <= 4096
                    && (tail.is_empty()
                        || (suffix.starts_with('\n')
                            && !tail.contains('\n')
                            && tail.chars().next().is_some_and(char::is_alphabetic)))
                {
                    return Some((start, offset));
                }
                return None;
            }
        }
        boundary = line == "\n";
        offset += line.len();
    }
    None
}

//! Conservative append-only fences for streaming code blocks.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Ambiguous closers, indentation and normalized source stay with CommonMark.
#[derive(Debug)]
pub(super) struct OpenFence {
    marker: u8,
    length: usize,
    end: usize,
}

impl OpenFence {
    pub(super) fn detect(source: &str, end: usize) -> Option<Self> {
        let marker = *source.as_bytes().first()?;
        if !matches!(marker, b'`' | b'~')
            || !source.ends_with('\n')
            || source.contains(['\r', '\0'])
        {
            return None;
        }
        let (opening, code) = source.split_once('\n')?;
        let length = opening.bytes().take_while(|&byte| byte == marker).count();
        let info = opening[length..].trim();
        if length < 3 || info.contains(['&', '\\']) || (marker == b'`' && info.contains('`')) {
            return None;
        }
        let fence = Self {
            marker,
            length,
            end,
        };
        (!fence.maybe_closes(code)).then_some(fence)
    }

    pub(super) fn append<'a>(&mut self, source: &'a str) -> Option<&'a str> {
        let delta = source.get(self.end..)?;
        if !source.ends_with('\n') || delta.contains(['\r', '\0']) || self.maybe_closes(delta) {
            return None;
        }
        self.end = source.len();
        Some(delta)
    }

    fn maybe_closes(&self, source: &str) -> bool {
        source.lines().any(|line| {
            line.trim_start_matches([' ', '\t'])
                .bytes()
                .take_while(|&byte| byte == self.marker)
                .count()
                >= self.length
        })
    }
}

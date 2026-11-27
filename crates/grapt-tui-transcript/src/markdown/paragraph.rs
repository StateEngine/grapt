//! Conservative literal paragraph appends extend the final wrapped row.
//! Markdown punctuation, blank lines and whitespace-sensitive syntax return
//! to the full parser, which can reinterpret any previously rendered block.
#[derive(Debug)]
pub(super) struct OpenParagraph {
    end: usize,
}

impl OpenParagraph {
    pub(super) fn detect(source: &str, end: usize) -> Option<Self> {
        plain(source).then_some(Self { end })
    }

    pub(super) fn append<'a>(&mut self, source: &'a str) -> Option<&'a str> {
        let delta = source.get(self.end..)?;
        if !plain(delta) {
            return None;
        }
        self.end = source.len();
        Some(delta)
    }
}

fn plain(source: &str) -> bool {
    source.ends_with('\n')
        && source.split_terminator('\n').all(|line| {
            line.chars().next().is_some_and(char::is_alphabetic)
                && !line.chars().last().is_some_and(char::is_whitespace)
                && !line.contains([
                    '\r', '\t', '\\', '`', '*', '_', '[', ']', '<', '>', '&', '~', '|',
                ])
        })
}

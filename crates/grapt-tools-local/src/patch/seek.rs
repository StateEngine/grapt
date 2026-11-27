//! Copyright 2025 OpenAI.
//! Licensed under Apache-2.0; see licenses/openai-codex.
//! Modified by Grapt: bounded matching and the existing governed patch executor.
//! Four matching passes, with a finite comparison/byte allowance.

pub(super) struct WorkBudget {
    comparisons: usize,
    bytes: usize,
}
impl WorkBudget {
    pub(super) fn new() -> Self {
        Self {
            comparisons: 1_000_000,
            bytes: 64 * 1024 * 1024,
        }
    }
    fn charge(&mut self, left: &str, right: &str) -> Result<(), String> {
        let error = || "patch matching work exceeded; narrow the @@ context".to_owned();
        let bytes = left
            .len()
            .checked_add(right.len())
            .ok_or_else(error)?
            .max(1);
        self.comparisons = self.comparisons.checked_sub(1).ok_or_else(error)?;
        self.bytes = self.bytes.checked_sub(bytes).ok_or_else(error)?;
        Ok(())
    }
}

pub(super) fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    eof: bool,
    work: &mut WorkBudget,
) -> Result<Option<usize>, String> {
    if pattern.is_empty() {
        return Ok(Some(start));
    }
    if pattern.len() > lines.len() {
        return Ok(None);
    }
    let search_start = if eof {
        (lines.len() - pattern.len()).max(start)
    } else {
        start
    };
    fn normalise(s: &str) -> String {
        s.trim()
            .chars()
            .map(|c| match c {
                // Various dash / hyphen code-points → ASCII '-'
                '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
                | '\u{2212}' => '-',
                // Fancy single quotes → '\''
                '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                // Fancy double quotes → '"'
                '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
                // Non-breaking space and other odd spaces → normal space
                '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
                | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
                | '\u{3000}' => ' ',
                other => other,
            })
            .collect::<String>()
    }

    for pass in 0..4 {
        for i in search_start..=lines.len() - pattern.len() {
            let mut matched = true;
            for (j, expected) in pattern.iter().enumerate() {
                let actual = &lines[i + j];
                work.charge(actual, expected)?;
                let equal = match pass {
                    0 => actual == expected,
                    1 => actual.trim_end() == expected.trim_end(),
                    2 => actual.trim() == expected.trim(),
                    _ => normalise(actual) == normalise(expected),
                };
                if !equal {
                    matched = false;
                    break;
                }
            }
            if matched {
                return Ok(Some(i));
            }
        }
    }
    Ok(None)
}

//! The characters a terminal would show, rebuilt from the TUI's output.
//!
//! The TUI redraws only changed cells, so a word can arrive split around a
//! cursor move. This keeps a character grid and applies cursor addressing,
//! erases and text; colors, modes and other controls change no character and
//! are skipped. Journeys ask only whether text is on screen, never where.

pub(super) struct Screen {
    rows: Vec<Vec<char>>,
    row: usize,
    column: usize,
    pending: Vec<u8>,
}

impl Screen {
    pub(super) fn new(rows: u16, columns: u16) -> Self {
        Self {
            rows: vec![vec![' '; usize::from(columns)]; usize::from(rows)],
            row: 0,
            column: 0,
            pending: Vec::new(),
        }
    }

    pub(super) fn text(&self) -> String {
        let mut text = String::new();
        for row in &self.rows {
            text.extend(row.iter());
            text.push('\n');
        }
        text
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let input = std::mem::take(&mut self.pending);
        let mut at = 0;
        while at < input.len() {
            match self.step(&input[at..]) {
                Some(used) => at += used,
                // An escape or character split across reads waits for the rest.
                None => {
                    self.pending = input[at..].to_vec();
                    return;
                }
            }
        }
    }

    /// Applies one control, escape or character; None when it is incomplete.
    fn step(&mut self, input: &[u8]) -> Option<usize> {
        match input[0] {
            0x1b => self.escape(input),
            b'\r' => {
                self.column = 0;
                Some(1)
            }
            b'\n' => {
                self.line_feed();
                Some(1)
            }
            0x08 => {
                self.column = self.column.saturating_sub(1);
                Some(1)
            }
            byte if byte < 0x20 || byte == 0x7f => Some(1),
            _ => {
                let width = match input[0] {
                    byte if byte < 0x80 => 1,
                    byte if byte >= 0xf0 => 4,
                    byte if byte >= 0xe0 => 3,
                    _ => 2,
                };
                let bytes = input.get(..width)?;
                let character = std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|text| text.chars().next())
                    .unwrap_or('\u{fffd}');
                self.put(character);
                Some(width)
            }
        }
    }

    fn escape(&mut self, input: &[u8]) -> Option<usize> {
        match *input.get(1)? {
            b'[' => {
                let end = input[2..]
                    .iter()
                    .position(|byte| (0x40..=0x7e).contains(byte))?
                    + 2;
                self.control(&input[2..end], input[end]);
                Some(end + 1)
            }
            b']' | b'P' | b'_' => {
                // Strings end at BEL or ST; they never draw characters.
                let body = &input[2..];
                if let Some(bell) = body.iter().position(|byte| *byte == 0x07) {
                    return Some(bell + 3);
                }
                body.windows(2)
                    .position(|pair| pair == b"\x1b\\")
                    .map(|st| st + 4)
            }
            b'(' | b')' => input.get(2).map(|_| 3),
            _ => Some(2),
        }
    }

    fn control(&mut self, parameters: &[u8], command: u8) {
        if parameters
            .first()
            .is_some_and(|byte| matches!(byte, b'?' | b'>' | b'<' | b'='))
        {
            return;
        }
        let numbers: Vec<usize> = std::str::from_utf8(parameters)
            .unwrap_or("")
            .split(';')
            .map(|part| part.parse().unwrap_or(0))
            .collect();
        let first = numbers.first().copied().unwrap_or(0);
        let count = first.max(1);
        let last_row = self.rows.len() - 1;
        let last_column = self.rows[0].len() - 1;
        match command {
            b'H' | b'f' => {
                self.row = first.max(1).min(self.rows.len()) - 1;
                self.column = numbers
                    .get(1)
                    .copied()
                    .unwrap_or(1)
                    .max(1)
                    .min(last_column + 1)
                    - 1;
            }
            b'A' => self.row = self.row.saturating_sub(count),
            b'B' => self.row = (self.row + count).min(last_row),
            b'C' => self.column = (self.column + count).min(last_column),
            b'D' => self.column = self.column.saturating_sub(count),
            b'G' => self.column = count.min(last_column + 1) - 1,
            b'd' => self.row = count.min(last_row + 1) - 1,
            b'K' => {
                let row = &mut self.rows[self.row];
                let range = match first {
                    1 => 0..=self.column.min(last_column),
                    2 => 0..=last_column,
                    _ => self.column.min(last_column)..=last_column,
                };
                row[range].fill(' ');
            }
            b'J' => {
                let (from, to) = match first {
                    1 => (0, self.row),
                    2 | 3 => (0, last_row),
                    _ => (self.row, last_row),
                };
                for row in &mut self.rows[from..=to] {
                    row.fill(' ');
                }
            }
            b'X' => {
                let end = (self.column + count).min(last_column + 1);
                self.rows[self.row][self.column.min(last_column)..end].fill(' ');
            }
            _ => {}
        }
    }

    fn put(&mut self, character: char) {
        let last_column = self.rows[0].len() - 1;
        if self.column > last_column {
            self.column = 0;
            self.line_feed();
        }
        self.rows[self.row][self.column] = character;
        self.column += 1;
    }

    fn line_feed(&mut self) {
        if self.row + 1 < self.rows.len() {
            self.row += 1;
        } else {
            self.rows.remove(0);
            let columns = self.rows[0].len();
            self.rows.push(vec![' '; columns]);
        }
    }
}

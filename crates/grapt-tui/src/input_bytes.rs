//! R's extra terminal keys on the existing bounded VT/paste parser.
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use grapt_client::{
    vt::{VtEvent, VtParser},
    Key,
};
use std::time::Duration;

mod discard;
use discard::Discard;

#[cfg(test)]
#[path = "input_bytes/control_tests.rs"]
mod control_tests;

pub struct Parser {
    vt: VtParser,
    control: Vec<u8>,
    escape_at: Option<Duration>,
    discard: Option<Discard>,
}

impl Parser {
    pub fn new() -> Self {
        Self {
            vt: VtParser::with_limit(crate::MAX_DRAFT),
            control: Vec::new(),
            escape_at: None,
            discard: None,
        }
    }

    pub fn idle(&self) -> bool {
        self.control.is_empty() && self.vt.idle()
    }

    pub fn wait(&self, now: Duration) -> Option<Duration> {
        if !self.control.is_empty() {
            return self
                .escape_at
                .map(|at| Duration::from_millis(100).saturating_sub(now.saturating_sub(at)));
        }
        self.vt.escape_wait(now)
    }

    pub fn expire(&mut self, now: Duration) -> Vec<Result<Event, String>> {
        if self.wait(now) != Some(Duration::ZERO) {
            return vec![];
        }
        if self.control == b"\x1b" {
            self.control.clear();
            self.escape_at = None;
            return vec![Ok(Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )))];
        }
        if !self.control.is_empty() {
            self.discard = Discard::unfinished(&self.control);
            self.control.clear();
            self.escape_at = None;
            return vec![Err(
                "Incomplete terminal control sequence dropped; draft kept".into(),
            )];
        }
        self.vt
            .flush_expired_escape(now)
            .map(convert)
            .into_iter()
            .collect()
    }

    pub fn feed(&mut self, bytes: &[u8], now: Duration) -> Vec<Result<Event, String>> {
        let mut output = Vec::new();
        for byte in bytes {
            if let Some(discard) = self.discard.take() {
                // A fresh introducer resynchronizes immediately. Otherwise
                // consume the refused report's tail without buffering it.
                if *byte != 0x1b {
                    self.discard = discard.consume(*byte);
                    continue;
                }
            }
            if self.control.is_empty() {
                if *byte == 0x1b && self.vt.idle() {
                    self.control.push(*byte);
                    self.escape_at = Some(now);
                } else if byte.is_ascii() && self.vt.idle() {
                    // R's raw-mode editor uses Ctrl+J/H and the full control
                    // alphabet. The shared TUI deliberately maps fewer keys.
                    if let Ok(Some(key)) = scalar(&[*byte]) {
                        output.push(Ok(Event::Key(key)));
                    }
                } else {
                    output.extend(self.vt.feed_at(&[*byte], now).into_iter().map(convert));
                }
                continue;
            }
            if *byte == 0x1b && self.control.len() > 1 {
                self.control.clear();
                self.control.push(*byte);
                self.escape_at = Some(now);
                output.push(Err(
                    "Incomplete terminal control sequence dropped; draft kept".into(),
                ));
                continue;
            }
            self.control.push(*byte);
            if self.control.len() > 128 {
                if !(0x40..=0x7e).contains(byte) {
                    self.discard = Discard::unfinished(&self.control);
                }
                self.control.clear();
                self.escape_at = None;
                output.push(Err(
                    "Terminal control sequence exceeds its bound; draft kept".into(),
                ));
                continue;
            }
            if !matches!(self.control.get(1), Some(b'[' | b'O')) {
                // The first Escape is a complete key; the second may begin a
                // function/navigation key arriving in the same terminal read.
                if self.control == b"\x1b\x1b" {
                    output.push(Ok(Event::Key(KeyEvent::new(
                        KeyCode::Esc,
                        KeyModifiers::NONE,
                    ))));
                    self.control.truncate(1);
                    self.escape_at = Some(now);
                    continue;
                }
                let parsed = scalar(&self.control[1..]).map(|key| {
                    key.map(|mut key| {
                        key.modifiers |= KeyModifiers::ALT;
                        key
                    })
                });
                match parsed {
                    Ok(None) => continue,
                    Ok(Some(key)) => output.push(Ok(Event::Key(key))),
                    Err(()) => {
                        output.push(Err("Terminal key is not valid UTF-8; draft kept".into()))
                    }
                }
                self.control.clear();
                self.escape_at = None;
                continue;
            }
            let complete = match self.control.as_slice() {
                [0x1b, b'[', b'M', ..] => self.control.len() == 6,
                [0x1b, b'[' | b'O'] => false,
                [0x1b, _] => true,
                [0x1b, b'O', ..] => true,
                [0x1b, b'[', ..] => (0x40..=0x7e).contains(byte),
                _ => true,
            };
            if !complete {
                continue;
            }
            let extra = extra(&self.control);
            if self.control == b"\x1b[I" {
                output.push(Ok(Event::FocusGained));
            } else if self.control == b"\x1b[O" {
                output.push(Ok(Event::FocusLost));
            } else if self.control.starts_with(b"\x1b[<") || self.control.starts_with(b"\x1b[M") {
                match crate::mouse::decode(&self.control) {
                    Ok(Some(event)) => output.push(Ok(event)),
                    Ok(None) => {}
                    Err(error) => output.push(Err(error)),
                }
            } else if let Some(event) = extra {
                output.push(Ok(Event::Key(event)));
            } else {
                output.extend(self.vt.feed_at(&self.control, now).into_iter().map(convert));
            }
            self.control.clear();
            self.escape_at = None;
        }
        output
    }
}

// Match crossterm's raw Unix scalar semantics, including split Alt+UTF-8.
// CSI/SS3 and bracketed paste stay with their bounded parsers above.
fn scalar(bytes: &[u8]) -> Result<Option<KeyEvent>, ()> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => return Ok(None),
        Err(_) => return Err(()),
    };
    let ch = text.chars().next().ok_or(())?;
    let (code, modifiers) = match ch {
        '\r' => (KeyCode::Enter, KeyModifiers::NONE),
        '\t' => (KeyCode::Tab, KeyModifiers::NONE),
        '\x7f' => (KeyCode::Backspace, KeyModifiers::NONE),
        '\x01'..='\x1a' => (
            KeyCode::Char(char::from(ch as u8 - 1 + b'a')),
            KeyModifiers::CONTROL,
        ),
        '\x1c'..='\x1f' => (
            KeyCode::Char(char::from(ch as u8 - 0x1c + b'4')),
            KeyModifiers::CONTROL,
        ),
        '\0' => (KeyCode::Char(' '), KeyModifiers::CONTROL),
        _ => (KeyCode::Char(ch), character_modifiers(ch)),
    };
    Ok(Some(KeyEvent::new(code, modifiers)))
}

fn character_modifiers(ch: char) -> KeyModifiers {
    if ch.is_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    }
}

fn extra(bytes: &[u8]) -> Option<KeyEvent> {
    let (code, modifiers) = match bytes {
        b"\x1bOP" => (KeyCode::F(1), KeyModifiers::NONE),
        b"\x1bOA" => (KeyCode::Up, KeyModifiers::NONE),
        b"\x1bOB" => (KeyCode::Down, KeyModifiers::NONE),
        b"\x1bOC" => (KeyCode::Right, KeyModifiers::NONE),
        b"\x1bOD" => (KeyCode::Left, KeyModifiers::NONE),
        b"\x1bOH" => (KeyCode::Home, KeyModifiers::NONE),
        b"\x1bOF" => (KeyCode::End, KeyModifiers::NONE),
        b"\x1bOM" => (KeyCode::Enter, KeyModifiers::NONE),
        b"\x1b[Z" => (KeyCode::BackTab, KeyModifiers::SHIFT),
        b"\x1bOQ" => (KeyCode::F(2), KeyModifiers::NONE),
        b"\x1bOR" => (KeyCode::F(3), KeyModifiers::NONE),
        b"\x1bOS" => (KeyCode::F(4), KeyModifiers::NONE),
        b"\x1b[1;5H" | b"\x1b[7;5~" => (KeyCode::Home, KeyModifiers::CONTROL),
        b"\x1b[1;5F" | b"\x1b[8;5~" => (KeyCode::End, KeyModifiers::CONTROL),
        b"\x1b[13;2u" | b"\x1b[27;2;13~" => (KeyCode::Enter, KeyModifiers::SHIFT),
        b"\x1b[1;3B" => (KeyCode::Down, KeyModifiers::ALT),
        _ => return modified(bytes),
    };
    Some(KeyEvent::new(code, modifiers))
}

// Adapted from crossterm-rs/crossterm src/event/sys/unix/parse.rs at
// 36d95b26a26e64b0f8c12edfe11f410a6d56a812. Copyright 2019 Timon, MIT;
// licenses/crossterm/LICENSE. Modified for this bounded VT decoder.
fn modified(bytes: &[u8]) -> Option<KeyEvent> {
    let text = std::str::from_utf8(bytes).ok()?.strip_prefix("\x1b[")?;
    let parameters = text.get(..text.len().checked_sub(1)?)?;
    let (head, modifier) = parameters.split_once(';').unwrap_or((parameters, "1"));
    let value: u8 = modifier.parse().ok()?;
    if !(1..=8).contains(&value) {
        return None;
    }
    let bits = value - 1;
    let mut modifiers = KeyModifiers::NONE;
    if bits & 1 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if bits & 2 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if bits & 4 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    let code = match (head, bytes.last().copied()?) {
        ("1", b'A') => KeyCode::Up,
        ("1", b'B') => KeyCode::Down,
        ("1", b'C') => KeyCode::Right,
        ("1", b'D') => KeyCode::Left,
        ("1", b'H') => KeyCode::Home,
        ("1", b'F') => KeyCode::End,
        ("1", b'P') => KeyCode::F(1),
        ("1", b'Q') => KeyCode::F(2),
        ("1", b'R') => KeyCode::F(3),
        ("1", b'S') => KeyCode::F(4),
        ("3", b'~') => KeyCode::Delete,
        ("5", b'~') => KeyCode::PageUp,
        ("6", b'~') => KeyCode::PageDown,
        (head, b'~') => {
            let number = match head.parse::<u8>().ok()? {
                value @ 11..=15 => value - 10,
                value @ 17..=21 => value - 11,
                value @ 23..=26 => value - 12,
                value @ 28..=29 => value - 15,
                value @ 31..=34 => value - 17,
                _ => return None,
            };
            KeyCode::F(number)
        }
        (head, b'u') => {
            let code = head.parse::<u16>().ok()?;
            if !(57364..=57387).contains(&code) {
                return None;
            }
            KeyCode::F(u8::try_from(code - 57363).ok()?)
        }
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

pub fn convert(event: VtEvent) -> Result<Event, String> {
    let key = match event {
        VtEvent::Paste(text) => return Ok(Event::Paste(text)),
        VtEvent::PasteRefused(reason) => return Err(reason.reason()),
        VtEvent::Key(key) => key,
    };
    let (code, modifiers) = match key {
        Key::Char(ch) => (KeyCode::Char(ch), character_modifiers(ch)),
        Key::Ctrl(ch) => (KeyCode::Char(ch), KeyModifiers::CONTROL),
        Key::Alt(ch) => (KeyCode::Char(ch), KeyModifiers::ALT),
        Key::Enter => (KeyCode::Enter, KeyModifiers::NONE),
        Key::Escape => (KeyCode::Esc, KeyModifiers::NONE),
        Key::Backspace => (KeyCode::Backspace, KeyModifiers::NONE),
        Key::Tab => (KeyCode::Tab, KeyModifiers::NONE),
        Key::Up => (KeyCode::Up, KeyModifiers::NONE),
        Key::Down => (KeyCode::Down, KeyModifiers::NONE),
        Key::Left => (KeyCode::Left, KeyModifiers::NONE),
        Key::Right => (KeyCode::Right, KeyModifiers::NONE),
        Key::Home => (KeyCode::Home, KeyModifiers::NONE),
        Key::End => (KeyCode::End, KeyModifiers::NONE),
        Key::Delete => (KeyCode::Delete, KeyModifiers::NONE),
        Key::CtrlLeft => (KeyCode::Left, KeyModifiers::CONTROL),
        Key::CtrlRight => (KeyCode::Right, KeyModifiers::CONTROL),
        Key::AltBackspace => (KeyCode::Backspace, KeyModifiers::ALT),
        Key::PageUp => (KeyCode::PageUp, KeyModifiers::NONE),
        Key::PageDown => (KeyCode::PageDown, KeyModifiers::NONE),
    };
    Ok(Event::Key(KeyEvent::new(code, modifiers)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_f8_expands_automated_input_but_pasted_bytes_stay_text() {
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b[1", Duration::ZERO).is_empty());
        assert_eq!(
            parser.feed(b"9~", Duration::ZERO),
            [Ok(Event::Key(KeyEvent::new(
                KeyCode::F(8),
                KeyModifiers::NONE
            )))]
        );
        assert_eq!(
            parser.feed(b"\x1b[200~literal \x1b[19~\x1b[201~", Duration::ZERO),
            [Ok(Event::Paste("literal \x1b[19~".into()))]
        );
        assert!(parser.idle());
    }

    #[test]
    fn fragmented_focus_reports_are_events_and_pasted_reports_remain_text() {
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b[", Duration::ZERO).is_empty());
        assert_eq!(parser.feed(b"O", Duration::ZERO), [Ok(Event::FocusLost)]);
        assert_eq!(
            parser.feed(b"\x1b[I", Duration::ZERO),
            [Ok(Event::FocusGained)]
        );
        assert_eq!(
            parser.feed(b"\x1b[200~literal \x1b[O\x1b[201~", Duration::ZERO),
            [Ok(Event::Paste("literal \x1b[O".into()))],
        );
        assert!(parser.idle());
    }

    #[test]
    fn escape_followed_by_a_function_key_keeps_its_second_introducer() {
        let mut parser = Parser::new();
        assert_eq!(
            parser.feed(b"\x1b\x1b[17~", Duration::ZERO),
            [
                Ok(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
                Ok(Event::Key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE))),
            ]
        );
        assert!(parser.idle());
        assert!(parser.expire(Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn raw_editor_controls_and_alt_scalars_keep_crossterm_semantics() {
        for (bytes, code, modifiers) in [
            (b"\n".as_slice(), KeyCode::Char('j'), KeyModifiers::CONTROL),
            (b"\x08", KeyCode::Char('h'), KeyModifiers::CONTROL),
            (b"\0", KeyCode::Char(' '), KeyModifiers::CONTROL),
            (b"\x1c", KeyCode::Char('4'), KeyModifiers::CONTROL),
            (b"\x1f", KeyCode::Char('7'), KeyModifiers::CONTROL),
            (b"\r", KeyCode::Enter, KeyModifiers::NONE),
            (b"\t", KeyCode::Tab, KeyModifiers::NONE),
            (b"\x1bh", KeyCode::Char('h'), KeyModifiers::ALT),
            (
                b"\x1b\n",
                KeyCode::Char('j'),
                KeyModifiers::ALT | KeyModifiers::CONTROL,
            ),
            (
                b"\x1bH",
                KeyCode::Char('H'),
                KeyModifiers::ALT | KeyModifiers::SHIFT,
            ),
            (b"\x1b\x7f", KeyCode::Backspace, KeyModifiers::ALT),
            (
                "\x1bŽ".as_bytes(),
                KeyCode::Char('Ž'),
                KeyModifiers::ALT | KeyModifiers::SHIFT,
            ),
            ("Ž".as_bytes(), KeyCode::Char('Ž'), KeyModifiers::SHIFT),
        ] {
            let mut parser = Parser::new();
            let mut events = Vec::new();
            for (index, byte) in bytes.iter().enumerate() {
                events.extend(parser.feed(&[*byte], Duration::ZERO));
                if index + 1 < bytes.len() {
                    assert!(
                        !parser.idle(),
                        "split key was acknowledged early: {bytes:?}"
                    );
                }
            }
            assert_eq!(events, [Ok(Event::Key(KeyEvent::new(code, modifiers)))]);
            assert!(parser.idle());
        }
    }

    #[test]
    fn retry_followed_by_an_unfinished_paste_keeps_the_boundary_closed() -> Result<(), String> {
        let mut parser = Parser::new();
        let events = parser.feed(b"\x0f\x1b[200~unfinished", Duration::ZERO);
        assert_eq!(
            events,
            [Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL
            )))]
        );
        assert!(!parser.idle());
        assert!(parser.expire(Duration::from_secs(10)).is_empty());
        assert_eq!(
            parser.feed(b" paste\x1b[201~", Duration::from_secs(10)),
            [Ok(Event::Paste("unfinished paste".into()))]
        );
        assert!(parser.idle());
        Ok(())
    }

    #[test]
    fn split_escape_keys_and_r_shortcuts_keep_their_meaning() -> Result<(), String> {
        for (bytes, key) in [
            (
                b"\x1bOR".as_slice(),
                KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE),
            ),
            (
                b"\x1b[1;5H",
                KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL),
            ),
            (
                b"\x1b[13;2u",
                KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            ),
            (b"\x1b[D", KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
        ] {
            let mut parser = Parser::new();
            let mut events = Vec::new();
            for byte in bytes {
                events.extend(parser.feed(&[*byte], Duration::ZERO));
            }
            assert_eq!(events, [Ok(Event::Key(key))]);
            assert!(parser.idle());
        }
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b", Duration::ZERO).is_empty());
        assert!(!parser.idle());
        assert_eq!(
            parser.expire(Duration::from_millis(100)),
            [Ok(Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE
            )))]
        );
        assert!(parser.idle());
        Ok(())
    }
}

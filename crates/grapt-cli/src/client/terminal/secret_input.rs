//! Read an API key from the terminal without echo (J01).
//!
//! Raw mode is entered here and only here, through `crossterm` (the edge
//! admitted for this; the workspace forbids the `unsafe` a termios or console
//! call would need). A guard leaves raw mode on every exit path: a return, an
//! error, and a panic unwind. Ctrl-C does not raise a signal in raw mode; it
//! arrives as a byte and cancels. The key is never echoed, printed, or placed
//! in an error.

use std::io::{IsTerminal, Read, Write};

/// The longest key accepted. Real API keys are far shorter; a longer paste is
/// not a key.
pub(super) const MAX_SECRET_BYTES: usize = 512;

/// Leaves raw mode when dropped, however the read ended.
struct RawMode;

impl RawMode {
    fn enter() -> Result<Self, String> {
        crossterm::terminal::enable_raw_mode()
            .map_err(|_| "the terminal cannot hide input; nothing was created".to_owned())?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _restored = crossterm::terminal::disable_raw_mode();
    }
}

/// Why a secret was not read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refused {
    Cancelled,
    TooLong,
    Empty,
    NotText,
    /// The terminal ended, or could not be read, before Enter.
    Interrupted,
}

impl Refused {
    pub(super) fn reason(&self) -> &'static str {
        match self {
            Self::Cancelled => "setup cancelled; nothing was created",
            Self::TooLong => "that is longer than any API key; nothing was created",
            Self::Empty => "no key was entered; nothing was created",
            Self::NotText => "the key is not text; nothing was created",
            Self::Interrupted => {
                "the terminal closed before the key was entered; nothing was created"
            }
        }
    }
}

/// Apply terminal keystrokes, as raw bytes, to an empty line: printable
/// bytes append, Backspace/Delete remove the last character, Enter ends,
/// Ctrl-C and Ctrl-D cancel, escape sequences (arrow and function keys) and
/// other control bytes are ignored. Only Enter accepts: input that ends
/// first (the terminal closed, or a read failed) refuses, whatever was typed.
/// A line longer than `max` is not stored past `max`; the rest of it is
/// read and discarded, still hidden, up to its Enter, and then refused, so
/// no tail of an oversized paste reaches a visible terminal.
pub(super) fn edited(bytes: impl IntoIterator<Item = u8>, max: usize) -> Result<String, Refused> {
    #[derive(PartialEq)]
    enum Escape {
        None,
        Started,
        Sequence,
    }
    let mut line: Vec<u8> = Vec::new();
    let mut escape = Escape::None;
    let mut overlong = false;
    let mut entered = false;
    for byte in bytes {
        match escape {
            Escape::Started => {
                escape = if byte == b'[' || byte == b'O' {
                    Escape::Sequence
                } else {
                    Escape::None
                };
                continue;
            }
            Escape::Sequence => {
                if (0x40..=0x7e).contains(&byte) {
                    escape = Escape::None;
                }
                continue;
            }
            Escape::None => {}
        }
        match byte {
            0x1b => escape = Escape::Started,
            b'\r' | b'\n' => {
                entered = true;
                break;
            }
            0x03 | 0x04 => return Err(Refused::Cancelled),
            0x08 | 0x7f => {
                // Remove one whole UTF-8 character, not one byte of it.
                while let Some(last) = line.pop() {
                    if last & 0xc0 != 0x80 {
                        break;
                    }
                }
            }
            byte if byte < 0x20 => {}
            _ if overlong => {}
            byte => {
                if line.len() == max {
                    overlong = true;
                } else {
                    line.push(byte);
                }
            }
        }
    }
    if !entered {
        return Err(Refused::Interrupted);
    }
    if overlong {
        return Err(Refused::TooLong);
    }
    let text = String::from_utf8(line).map_err(|_| Refused::NotText)?;
    let text = text.trim().to_owned();
    if text.is_empty() {
        Err(Refused::Empty)
    } else {
        Ok(text)
    }
}

/// Retry accidental empty entries without leaving the hidden-input mode.
/// The caller owns raw mode for this entire loop, including prompt retries.
fn nonempty(
    bytes: &mut impl Iterator<Item = u8>,
    prompt: &str,
    out: &mut impl Write,
) -> Result<String, Refused> {
    loop {
        match edited(bytes.by_ref(), MAX_SECRET_BYTES) {
            Err(Refused::Empty) => {
                write!(out, "\r\nNo key was entered. Try again.\r\n{prompt}")
                    .and_then(|()| out.flush())
                    .map_err(|_| Refused::Interrupted)?;
            }
            result => return result,
        }
    }
}

/// Prompt on stderr and read a key from the terminal without echo.
/// Refuses, rather than reading a visible key, when stdin is not a terminal.
pub(super) fn read_secret(prompt: &str, permits_empty: bool) -> Result<String, String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err("an API key can only be entered at a terminal; nothing was created".into());
    }
    let mut out = std::io::stderr();
    let _ = write!(out, "{prompt}");
    let _ = out.flush();
    let result = {
        let _raw = RawMode::enter()?;
        let mut lock = stdin.lock();
        // End of input and a read error both end the bytes; `edited`
        // refuses either, since neither is Enter.
        let mut bytes = std::iter::from_fn(|| {
            let mut byte = [0u8];
            loop {
                match lock.read(&mut byte) {
                    Ok(1) => return Some(byte[0]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    _ => return None,
                }
            }
        });
        if permits_empty {
            match edited(&mut bytes, MAX_SECRET_BYTES) {
                Err(Refused::Empty) => Ok(String::new()),
                result => result,
            }
        } else {
            nonempty(&mut bytes, prompt, &mut out)
        }
    };
    let _ = writeln!(out);
    result.map_err(|refused| refused.reason().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(keys: &[u8]) -> Result<String, Refused> {
        edited(keys.iter().copied(), MAX_SECRET_BYTES)
    }

    #[test]
    fn keystrokes_edit_one_hidden_line() {
        assert_eq!(typed(b"sk-ant-abc\r").as_deref(), Ok("sk-ant-abc"));
        assert_eq!(typed(b"sk-abX\x7fc\n").as_deref(), Ok("sk-abc"));
        assert_eq!(
            typed(b"sk-ab\x08\x08\x08\x08\x08\x08sk-1\r").as_deref(),
            Ok("sk-1")
        );
        assert_eq!(
            typed(b"  sk-1 \x1b[A\x1b[3~\x1bOP\r").as_deref(),
            Ok("sk-1")
        );
        assert_eq!(typed("sk-Ωx\x7f\x7f1\r".as_bytes()).as_deref(), Ok("sk-1"));
    }

    #[test]
    fn ctrl_c_ctrl_d_cancel_and_an_empty_line_is_distinct() {
        assert_eq!(typed(b"sk-1\x03"), Err(Refused::Cancelled));
        assert_eq!(typed(b"\x04"), Err(Refused::Cancelled));
        assert_eq!(typed(b"   \r"), Err(Refused::Empty));
    }

    #[test]
    fn empty_entries_retry_the_same_provider_prompt_without_echoing_the_key() {
        let mut bytes = b"\r   \rsk-DUMMY-anthropic-key\r".iter().copied();
        let mut out = Vec::new();
        let prompt = "Anthropic API key (input hidden): ";
        assert_eq!(
            nonempty(&mut bytes, prompt, &mut out).as_deref(),
            Ok("sk-DUMMY-anthropic-key")
        );
        assert_eq!(
            out,
            format!("\r\nNo key was entered. Try again.\r\n{prompt}")
                .repeat(2)
                .as_bytes()
        );
    }

    #[test]
    fn cancellation_eof_and_oversized_keys_still_end_hidden_entry_after_a_retry() {
        for (keys, expected) in [
            (b"\rsk-DUMMY\x03".as_slice(), Refused::Cancelled),
            (b"\r\x04".as_slice(), Refused::Cancelled),
            (b"\r".as_slice(), Refused::Interrupted),
            (b"\rsk-DUMMY-partial".as_slice(), Refused::Interrupted),
        ] {
            let mut bytes = keys.iter().copied();
            assert_eq!(
                nonempty(&mut bytes, "same provider: ", &mut Vec::new()),
                Err(expected)
            );
        }
        let mut oversized = vec![b'\r'];
        oversized.extend(vec![b'x'; MAX_SECRET_BYTES + 1]);
        oversized.extend_from_slice(b"\rsk-DUMMY-next\r");
        let mut bytes = oversized.into_iter();
        assert_eq!(
            nonempty(&mut bytes, "same provider: ", &mut Vec::new()),
            Err(Refused::TooLong)
        );
        assert_eq!(bytes.collect::<Vec<_>>(), b"sk-DUMMY-next\r");
    }

    /// Only Enter accepts. The terminal closing (or a read failing) part-way
    /// through a key refuses instead of validating the partial key.
    #[test]
    fn input_that_ends_before_enter_refuses_the_partial_key() {
        assert_eq!(typed(b""), Err(Refused::Interrupted));
        assert_eq!(typed(b"sk-ant-partial-key"), Err(Refused::Interrupted));
        assert_eq!(typed(b"sk-ant-\x1b[A"), Err(Refused::Interrupted));
    }

    /// An oversized paste is consumed, hidden and unstored, through its own
    /// Enter, and only then refused: nothing of it is left to arrive after
    /// raw mode ends. Bytes after that Enter are not read.
    #[test]
    fn a_paste_longer_than_any_key_is_discarded_to_its_enter_and_refused() {
        let mut long = vec![b'k'; MAX_SECRET_BYTES + 1];
        long.extend_from_slice(b"TAIL-THAT-MUST-STAY-HIDDEN\r");
        let mut bytes = long.iter().copied().chain(*b"after\r");
        assert_eq!(
            edited(bytes.by_ref(), MAX_SECRET_BYTES),
            Err(Refused::TooLong)
        );
        assert_eq!(bytes.collect::<Vec<u8>>(), b"after\r");
        let mut cancelled = vec![b'k'; MAX_SECRET_BYTES + 10];
        cancelled.push(0x03);
        assert_eq!(typed(&cancelled), Err(Refused::Cancelled));
        let unfinished = vec![b'k'; MAX_SECRET_BYTES + 10];
        assert_eq!(typed(&unfinished), Err(Refused::Interrupted));
        let mut exact = vec![b'k'; MAX_SECRET_BYTES];
        exact.push(b'\r');
        assert_eq!(typed(&exact).map(|key| key.len()), Ok(MAX_SECRET_BYTES));
    }

    #[test]
    fn refusal_reasons_never_carry_the_key() {
        for refused in [
            Refused::Cancelled,
            Refused::TooLong,
            Refused::Empty,
            Refused::NotText,
            Refused::Interrupted,
        ] {
            assert!(refused.reason().ends_with("nothing was created"));
        }
    }
}

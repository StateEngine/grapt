//! The VT input parser for the Windows console path.
//!
//! ## Why this exists at all
//!
//! On Windows the client asks the console for `ENABLE_VIRTUAL_TERMINAL_INPUT`
//! and then reads stdin as BYTES, because crossterm 0.29's Windows event
//! translation is not usable for this seam. Measured under a real ConPTY with
//! an actual paste, three ways:
//!
//! - VT off, crossterm events: no paste event at all, and every pasted newline
//!   arrives as `Enter` — so a pasted two-line prompt SUBMITS its first line.
//!   `U+2014` and `U+2211` never arrive.
//! - VT on, crossterm events: still no paste event. The bracket markers appear
//!   as the bare characters `[200~` and `[201~` with THE ESC MISSING, so a
//!   paste is indistinguishable from a user typing `[200~`. The two characters
//!   are still lost.
//! - VT on, reading bytes: `1b 5b 32 30 30 7e`, the payload, then `ESC[201~`.
//!   Whole, correctly framed, and both characters intact.
//!
//! So the escape introducer is present the entire time and only the
//! console-to-`KeyEvent` translation destroys it. That is why the console-mode
//! toggle alone changes nothing, and why this parser owns the bytes instead.
//!
//! ## Why the parser is pure
//!
//! Nothing here touches a console, a handle or a thread: it is bytes in,
//! events out. The governed gate runs on Linux with no tty, so a parser that
//! needed a Windows console to be witnessed could not be witnessed at all.
//! Every rule below — chunk boundaries, partial UTF-8, oversize and
//! unterminated pastes — is therefore provable on any platform.

use crate::keymap::Key;
use std::time::Duration;

mod escape;

/// The largest paste this client will accept, in bytes.
///
/// A bound is required rather than nice: the terminal hands over a paste one
/// chunk at a time and the terminator may never arrive, so an unbounded
/// accumulator is a memory hole driven by whatever the user copied. Upstream
/// crossterm has this defect open at the time of writing — an unterminated
/// bracketed paste buffers indefinitely — and refusing is the honest answer.
pub const PASTE_LIMIT_BYTES: usize = 1_048_576;
const CONTROL_LIMIT_BYTES: usize = 128;

/// What the parser produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VtEvent {
    /// A key, in this crate's own vocabulary.
    Key(Key),
    /// A bracketed paste, whole and terminated.
    Paste(String),
    /// A paste this client will not deliver, and why.
    ///
    /// Refusing is deliberate. Truncating an oversize paste to its limit, or
    /// delivering an unterminated one as if it had ended, would put text into
    /// the composer that the user never finished pasting and cannot see is
    /// incomplete. A refusal the user can read is worse UX and better truth.
    PasteRefused(PasteRefusal),
}

/// Why a paste was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteRefusal {
    /// It exceeded [`PASTE_LIMIT_BYTES`] before its terminator arrived.
    TooLarge { bytes: usize, limit: usize },
    /// Invalid UTF-8 reached the composer's door.
    NotText,
}

impl PasteRefusal {
    /// The sentence a user should see. Says what happened and what to do.
    pub fn reason(&self) -> String {
        match self {
            Self::TooLarge { bytes, limit } => format!(
                "that paste is {bytes} bytes and this client accepts {limit}; \
                 nothing was inserted"
            ),
            Self::NotText => "that paste was not valid UTF-8 text; nothing was inserted".to_owned(),
        }
    }
}

/// Where the byte stream currently is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Ordinary input.
    Ground,
    /// An `ESC` was seen and its meaning is not yet decided.
    Escape,
    /// Inside a `CSI` sequence, accumulating it.
    Control(Vec<u8>),
    /// Oversized control sequences are consumed through their final byte.
    DiscardControl,
    /// Inside a bracketed paste.
    Paste,
    /// An `ESC` inside a paste, which may begin the terminator.
    PasteEscape,
    /// A `CSI` inside a paste, which may be the terminator.
    PasteControl(Vec<u8>),
}

/// Bytes in, events out.
#[derive(Debug)]
pub struct VtParser {
    state: State,
    escape_since: Option<Duration>,
    /// Bytes of a character that has not finished arriving.
    ///
    /// A multi-byte character can be split across reads — the terminal owes no
    /// alignment — so a parser that decoded each chunk independently would
    /// corrupt exactly the non-ASCII text this seam exists to carry.
    partial: Vec<u8>,
    paste: Vec<u8>,
    /// True once a paste has passed the limit: its bytes are then discarded
    /// rather than accumulated, but the terminator is still awaited so the
    /// stream resynchronises instead of spilling the remainder as keystrokes.
    paste_overflowed: bool,
    paste_bytes: usize,
    paste_after_carriage_return: bool,
    /// True immediately after a CR, so the LF of a CRLF pair does not submit a
    /// second time.
    after_carriage_return: bool,
    limit: usize,
}

impl Default for VtParser {
    fn default() -> Self {
        Self::new()
    }
}

impl VtParser {
    pub fn new() -> Self {
        Self::with_limit(PASTE_LIMIT_BYTES)
    }

    pub fn with_limit(limit: usize) -> Self {
        Self {
            state: State::Ground,
            escape_since: None,
            partial: Vec::new(),
            paste: Vec::new(),
            paste_overflowed: false,
            paste_bytes: 0,
            paste_after_carriage_return: false,
            after_carriage_return: false,
            limit,
        }
    }

    /// No partial character, control sequence or paste predates the next key.
    pub fn idle(&self) -> bool {
        self.state == State::Ground && self.partial.is_empty()
    }

    /// True while a paste is still arriving.
    ///
    /// The reader uses this to know that a quiet stdin means an UNFINISHED
    /// paste rather than an idle user.
    pub fn paste_in_flight(&self) -> bool {
        matches!(
            self.state,
            State::Paste | State::PasteEscape | State::PasteControl(_)
        )
    }

    /// Resolve a lone `ESC` that nothing followed.
    ///
    /// `ESC` alone and `ESC` as the introducer of a sequence are the same byte,
    /// so the difference is only knowable from what comes next — or from the
    /// fact that nothing did. The reader calls this when its wait expires, so a
    /// pressed Escape key is delivered on a quiet stream instead of being held
    /// until the user types something else.
    pub fn flush_pending_escape(&mut self) -> Option<VtEvent> {
        if self.state == State::Escape {
            self.escape_since = None;
            self.state = State::Ground;
            return Some(VtEvent::Key(Key::Escape));
        }
        None
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<VtEvent> {
        let mut events = Vec::new();
        for &byte in bytes {
            self.step(byte, &mut events);
        }
        events
    }

    fn step(&mut self, byte: u8, events: &mut Vec<VtEvent>) {
        match std::mem::replace(&mut self.state, State::Ground) {
            State::Ground => self.ground(byte, events),
            State::Escape => {
                if byte == b'[' {
                    self.state = State::Control(Vec::new());
                } else if byte == b'O' {
                    // Application-mode arrows/Home/End (SS3) share the same
                    // bounded control accumulator; unknown sequences stay out
                    // of the draft just like unknown CSI sequences.
                    self.state = State::Control(vec![b'O']);
                } else if matches!(byte, b'b' | b'f' | b'd' | b'r') {
                    events.push(VtEvent::Key(Key::Alt(char::from(byte))));
                } else if byte == 0x7f {
                    events.push(VtEvent::Key(Key::AltBackspace));
                } else {
                    // The ESC stood alone; this byte begins something new.
                    events.push(VtEvent::Key(Key::Escape));
                    self.ground(byte, events);
                }
            }
            State::Control(mut sequence) => {
                if is_final(byte) {
                    sequence.push(byte);
                    self.control(&sequence, events);
                } else if sequence.len() < CONTROL_LIMIT_BYTES {
                    sequence.push(byte);
                    self.state = State::Control(sequence);
                } else {
                    self.state = State::DiscardControl;
                }
            }
            State::DiscardControl => {
                if !is_final(byte) {
                    self.state = State::DiscardControl;
                }
            }
            State::Paste => {
                if byte == 0x1b {
                    self.state = State::PasteEscape;
                } else {
                    self.push_paste(byte);
                    self.state = State::Paste;
                }
            }
            State::PasteEscape => {
                if byte == b'[' {
                    self.state = State::PasteControl(Vec::new());
                } else {
                    // Not the terminator: the ESC was part of the pasted text.
                    self.push_paste(0x1b);
                    self.push_paste(byte);
                    self.state = State::Paste;
                }
            }
            State::PasteControl(mut sequence) => {
                sequence.push(byte);
                if b"201~".starts_with(&sequence) {
                    if sequence.as_slice() == b"201~" {
                        self.finish_paste(events);
                    } else {
                        self.state = State::PasteControl(sequence);
                    }
                } else {
                    // Only the four-byte terminator needs buffering. Other
                    // sequences belong to the bounded paste as literal bytes.
                    self.push_paste(0x1b);
                    self.push_paste(b'[');
                    for raw in sequence {
                        self.push_paste(raw);
                    }
                    self.state = State::Paste;
                }
            }
        }
    }

    fn ground(&mut self, byte: u8, events: &mut Vec<VtEvent>) {
        self.state = State::Ground;
        let after_cr = std::mem::take(&mut self.after_carriage_return);
        match byte {
            0x1b => self.state = State::Escape,
            // CR and LF are the same intent from a terminal. A CRLF pair must
            // not submit twice, so the LF that follows a CR is swallowed.
            b'\r' => {
                self.partial.clear();
                self.after_carriage_return = true;
                events.push(VtEvent::Key(Key::Enter));
            }
            b'\n' => {
                self.partial.clear();
                if !after_cr {
                    events.push(VtEvent::Key(Key::Enter));
                }
            }
            b'\t' => events.push(VtEvent::Key(Key::Tab)),
            0x08 | 0x7f => events.push(VtEvent::Key(Key::Backspace)),
            0x01..=0x1a => {
                let letter = (b'a' + (byte - 1)) as char;
                events.push(VtEvent::Key(Key::Ctrl(letter)));
            }
            _ => self.text(byte, events),
        }
    }

    /// Accumulate a character, which may span reads.
    fn text(&mut self, byte: u8, events: &mut Vec<VtEvent>) {
        self.partial.push(byte);
        match std::str::from_utf8(&self.partial) {
            Ok(text) => {
                for character in text.chars() {
                    events.push(VtEvent::Key(Key::Char(character)));
                }
                self.partial.clear();
            }
            Err(error) if error.error_len().is_none() => {
                // Incomplete but still valid so far: wait for the rest. A cap
                // stops a malformed stream from growing this forever; four
                // bytes is the longest any UTF-8 character can be.
                if self.partial.len() >= 4 {
                    self.partial.clear();
                }
            }
            Err(_) => {
                // Genuinely invalid. Drop it rather than inventing a character.
                self.partial.clear();
            }
        }
    }

    fn control(&mut self, sequence: &[u8], events: &mut Vec<VtEvent>) {
        self.state = State::Ground;
        match sequence {
            b"200~" => {
                self.state = State::Paste;
                self.paste.clear();
                self.paste_bytes = 0;
                self.paste_after_carriage_return = false;
                self.paste_overflowed = false;
                self.partial.clear();
            }
            b"A" | b"OA" => events.push(VtEvent::Key(Key::Up)),
            b"B" | b"OB" => events.push(VtEvent::Key(Key::Down)),
            b"D" | b"OD" => events.push(VtEvent::Key(Key::Left)),
            b"C" | b"OC" => events.push(VtEvent::Key(Key::Right)),
            b"H" | b"OH" | b"1~" | b"7~" => events.push(VtEvent::Key(Key::Home)),
            b"F" | b"OF" | b"4~" | b"8~" => events.push(VtEvent::Key(Key::End)),
            b"3~" => events.push(VtEvent::Key(Key::Delete)),
            b"1;5D" | b"1;3D" => events.push(VtEvent::Key(Key::CtrlLeft)),
            b"1;5C" | b"1;3C" => events.push(VtEvent::Key(Key::CtrlRight)),
            b"5~" => events.push(VtEvent::Key(Key::PageUp)),
            b"6~" => events.push(VtEvent::Key(Key::PageDown)),
            // Every other sequence is consumed and ignored rather than spilled
            // as keystrokes. A client that leaked `[`, `1`, `;` into the
            // composer on an unrecognised key would corrupt the user's prompt.
            _ => {}
        }
    }

    fn push_paste(&mut self, byte: u8) {
        self.paste_bytes = self.paste_bytes.saturating_add(1);
        if self.paste_bytes > self.limit {
            self.paste_overflowed = true;
            self.paste.clear();
            return;
        }
        // A pasted CR, or the CR of a CRLF, is a line break in text — not the
        // Enter key. Conflating them is the original defect.
        let after_cr = std::mem::replace(&mut self.paste_after_carriage_return, byte == b'\r');
        match byte {
            b'\r' => self.paste.push(b'\n'),
            b'\n' if after_cr => {}
            _ => self.paste.push(byte),
        }
    }

    fn finish_paste(&mut self, events: &mut Vec<VtEvent>) {
        self.state = State::Ground;
        if self.paste_overflowed {
            events.push(VtEvent::PasteRefused(PasteRefusal::TooLarge {
                bytes: self.paste_bytes,
                limit: self.limit,
            }));
        } else {
            match String::from_utf8(std::mem::take(&mut self.paste)) {
                Ok(text) => events.push(VtEvent::Paste(text)),
                Err(_) => events.push(VtEvent::PasteRefused(PasteRefusal::NotText)),
            }
        }
        self.paste.clear();
        self.paste_bytes = 0;
        self.paste_after_carriage_return = false;
        self.paste_overflowed = false;
    }
}

/// A CSI sequence ends at the first byte in this range.
fn is_final(byte: u8) -> bool {
    (0x40..=0x7e).contains(&byte)
}

#[cfg(test)]
mod tests;

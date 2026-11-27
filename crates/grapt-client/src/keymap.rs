//! Backend-independent key vocabulary shared by terminal input parsers.

/// One key press, in this crate's own vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
    CtrlLeft,
    CtrlRight,
    Alt(char),
    AltBackspace,
    PageUp,
    PageDown,
    /// A character pressed with the control modifier.
    Ctrl(char),
}

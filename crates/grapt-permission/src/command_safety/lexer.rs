//! A conservative word splitter for POSIX shells, PowerShell and cmd.
//!
//! It is not a shell parser: it finds the simple commands a text could run,
//! with their words after quote removal, the files their redirections write,
//! and whether their input is piped. Command substitutions, subshells and
//! script blocks become commands of their own, so nesting cannot hide one.
//! Each dialect quotes and escapes differently; the caller lexes every text
//! once per dialect and treats any reading as a command that may run.

/// The quoting and escaping rules a text is read with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Dialect {
    Posix,
    PowerShell,
    Cmd,
}

/// One word after quote removal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Word {
    pub text: String,
    /// Part of the word is produced at run time (`$(...)`, `<(...)`, backticks).
    pub dynamic: bool,
}

/// One simple command.
#[derive(Debug, Clone, Default)]
pub(super) struct Command {
    pub words: Vec<Word>,
    /// Files its output redirections write.
    pub writes: Vec<Word>,
    /// Its standard input is the previous command's output.
    pub piped: bool,
}

/// Lexing gave up: the text nests deeper than any reading is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TooDeep;

const MAX_NESTING: usize = 32;

pub(super) fn lex(text: &str, dialect: Dialect) -> Result<Vec<Command>, TooDeep> {
    let mut lexer = Lexer {
        chars: text.chars().collect(),
        at: 0,
        dialect,
        out: Vec::new(),
    };
    lexer.sequence(None, 0)?;
    Ok(lexer.out)
}

/// Where the unfinished end of typed input starts: after the last line end
/// that every dialect reads as ending a command line, not escaped, quoted or
/// commented away. A line continued by an escape or an open quote stays
/// unfinished, so it is read joined to the input that completes it.
pub(super) fn unfinished_start(text: &str) -> usize {
    let mut scans = [Dialect::Posix, Dialect::PowerShell, Dialect::Cmd].map(LineScan::new);
    let mut start = 0;
    let mut previous = None;
    for (at, ch) in text.char_indices() {
        let mut ends = true;
        for scan in &mut scans {
            ends &= scan.step(ch, previous);
        }
        if ends {
            start = at + ch.len_utf8();
        }
        previous = Some(ch);
    }
    start
}

/// One dialect's quote, escape and comment state across typed input.
struct LineScan {
    dialect: Dialect,
    quote: Option<char>,
    escaped: bool,
    comment: bool,
}

impl LineScan {
    fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            quote: None,
            escaped: false,
            comment: false,
        }
    }

    /// Whether `ch` ends a command line.
    fn step(&mut self, ch: char, previous: Option<char>) -> bool {
        let line_end = matches!(ch, '\n' | '\r');
        if std::mem::take(&mut self.escaped) {
            return false;
        }
        if self.comment {
            self.comment = !line_end;
            return line_end;
        }
        let escape = match self.dialect {
            Dialect::Posix => '\\',
            Dialect::PowerShell => '`',
            Dialect::Cmd => '^',
        };
        match self.quote {
            // cmd quotes end with the line.
            Some(_) if line_end && self.dialect == Dialect::Cmd => {
                self.quote = None;
                true
            }
            Some(quote) => {
                if ch == quote {
                    self.quote = None;
                } else if ch == escape && quote == '"' && self.dialect != Dialect::Cmd {
                    self.escaped = true;
                }
                false
            }
            None if ch == escape => {
                self.escaped = true;
                false
            }
            None if ch == '"' || (ch == '\'' && self.dialect != Dialect::Cmd) => {
                self.quote = Some(ch);
                false
            }
            None if ch == '#'
                && self.dialect != Dialect::Cmd
                && previous.is_none_or(|before| " \t\n\r;|&(".contains(before)) =>
            {
                self.comment = true;
                false
            }
            None => line_end,
        }
    }
}

struct Lexer {
    chars: Vec<char>,
    at: usize,
    dialect: Dialect,
    out: Vec<Command>,
}

/// The command being assembled at one nesting level.
#[derive(Default)]
struct Pending {
    command: Command,
    word: Word,
    started: bool,
}

impl Pending {
    fn end_word(&mut self) {
        if self.started {
            self.command.words.push(std::mem::take(&mut self.word));
            self.started = false;
        }
    }

    fn push(&mut self, ch: char) {
        self.word.text.push(ch);
        self.started = true;
    }
}

impl Lexer {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn finish(&mut self, pending: &mut Pending, piped_next: bool) {
        pending.end_word();
        let command = std::mem::take(&mut pending.command);
        if !command.words.is_empty() || !command.writes.is_empty() {
            self.out.push(command);
        }
        pending.command.piped = piped_next;
    }

    /// Lex commands until `close` (or the end), appending them to `out`.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn sequence(&mut self, close: Option<char>, depth: usize) -> Result<(), TooDeep> {
        if depth > MAX_NESTING {
            return Err(TooDeep);
        }
        let mut pending = Pending::default();
        while let Some(ch) = self.peek(0) {
            if Some(ch) == close && !self.brace_is_text(ch, &pending) {
                self.at += 1;
                break;
            }
            match ch {
                ',' | '=' | ';' if self.dialect == Dialect::Cmd => {
                    self.at += 1;
                    pending.end_word();
                }
                ' ' | '\t' => {
                    self.at += 1;
                    pending.end_word();
                }
                // Enter, Ctrl-C and Ctrl-D end a line typed into a terminal.
                '\n' | '\r' | '\u{3}' | '\u{4}' | ';' => {
                    self.at += 1;
                    self.finish(&mut pending, false);
                }
                '|' => {
                    self.at += 1;
                    let piped = match self.peek(0) {
                        Some('|') => {
                            self.at += 1;
                            false
                        }
                        Some('&') => {
                            self.at += 1;
                            true
                        }
                        _ => true,
                    };
                    self.finish(&mut pending, piped);
                }
                '&' if self.peek(1) == Some('>') && self.dialect == Dialect::Posix => {
                    self.at += 1;
                    pending.end_word();
                    self.redirect(&mut pending, depth)?;
                }
                '&' => {
                    self.at += 1;
                    if self.peek(0) == Some('&') {
                        self.at += 1;
                    }
                    self.finish(&mut pending, false);
                }
                '>' => {
                    // `2>` and PowerShell's `*>` name a stream, not a word.
                    if pending.started
                        && pending
                            .word
                            .text
                            .chars()
                            .all(|c| c.is_ascii_digit() || c == '*')
                    {
                        pending.word = Word::default();
                        pending.started = false;
                    }
                    pending.end_word();
                    self.redirect(&mut pending, depth)?;
                }
                '<' if self.peek(1) == Some('(') && self.dialect == Dialect::Posix => {
                    self.at += 2;
                    self.nested(&mut pending, ')', depth)?;
                }
                '<' => {
                    self.at += 1;
                    pending.end_word();
                    // Here-documents and here-strings are read as commands too:
                    // an interpreter would run them.
                    while self.peek(0) == Some('<') {
                        self.at += 1;
                    }
                }
                '{' | '}' if self.brace_is_text(ch, &pending) => {
                    self.word_char(&mut pending, depth)?;
                }
                '(' | '{' => {
                    self.at += 1;
                    self.finish(&mut pending, false);
                    self.sequence(Some(if ch == '(' { ')' } else { '}' }), depth + 1)?;
                }
                ')' | '}' => {
                    self.at += 1;
                    self.finish(&mut pending, false);
                }
                '#' if !pending.started && self.dialect != Dialect::Cmd => {
                    if self.dialect == Dialect::PowerShell
                        && self.at > 0
                        && self.chars[self.at - 1] == '<'
                    {
                        self.skip_block_comment();
                    } else {
                        while self.peek(0).is_some_and(|c| c != '\n') {
                            self.at += 1;
                        }
                    }
                }
                _ => self.word_char(&mut pending, depth)?,
            }
        }
        self.finish(&mut pending, false);
        Ok(())
    }

    /// POSIX `{` and `}` are reserved words, not operators: they group only
    /// where a command starts, and elsewhere -- `find -exec rm {} ;`,
    /// `xargs -I {}`, `{a,b}` -- they are ordinary word characters.
    /// PowerShell and cmd braces keep their own meaning.
    fn brace_is_text(&self, ch: char, pending: &Pending) -> bool {
        matches!(ch, '{' | '}')
            && self.dialect == Dialect::Posix
            && (pending.started || !pending.command.words.is_empty())
    }

    fn skip_block_comment(&mut self) {
        while let Some(ch) = self.peek(0) {
            self.at += 1;
            if ch == '#' && self.peek(0) == Some('>') {
                self.at += 1;
                return;
            }
        }
    }

    /// Read the target of an output redirection that starts at `>`.
    fn redirect(&mut self, pending: &mut Pending, depth: usize) -> Result<(), TooDeep> {
        while matches!(self.peek(0), Some('>' | '|')) {
            self.at += 1;
        }
        if self.peek(0) == Some('&') {
            // `>&2` duplicates a stream; `>& file` (POSIX) writes one.
            self.at += 1;
            if self.peek(0).is_some_and(|c| c.is_ascii_digit() || c == '-') {
                self.at += 1;
                return Ok(());
            }
        }
        while matches!(self.peek(0), Some(' ' | '\t')) {
            self.at += 1;
        }
        let mut target = Pending::default();
        while let Some(ch) = self.peek(0) {
            if matches!(
                ch,
                ' ' | '\t'
                    | '\r'
                    | '\n'
                    | '\u{3}'
                    | '\u{4}'
                    | ';'
                    | '|'
                    | '&'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '{'
                    | '}'
            ) {
                break;
            }
            self.word_char(&mut target, depth)?;
        }
        pending.command.writes.push(target.word);
        Ok(())
    }

    /// Run a nested sequence for a substitution inside the current word.
    fn nested(&mut self, pending: &mut Pending, close: char, depth: usize) -> Result<(), TooDeep> {
        pending.word.dynamic = true;
        pending.started = true;
        self.sequence(Some(close), depth + 1)
    }

    /// Consume one unquoted word character, or a whole quoted span.
    fn word_char(&mut self, pending: &mut Pending, depth: usize) -> Result<(), TooDeep> {
        let Some(ch) = self.peek(0) else {
            return Ok(());
        };
        self.at += 1;
        let drive = matches!(pending.word.text.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
        match (ch, self.dialect) {
            // `C:\work` is a Windows path even when read as POSIX.
            ('\\', Dialect::Posix) if drive => pending.push(ch),
            ('\\', Dialect::Posix) | ('`', Dialect::PowerShell) | ('^', Dialect::Cmd) => {
                match self.peek(0) {
                    Some('\n') => self.at += 1,
                    // A terminal turns Enter's `\r` into a line end.
                    Some('\r') => {
                        self.at += 1;
                        if self.peek(0) == Some('\n') {
                            self.at += 1;
                        }
                    }
                    Some(next) => {
                        self.at += 1;
                        pending.push(next);
                    }
                    None => {}
                }
            }
            ('`', Dialect::Posix) => {
                pending.word.text.push('$');
                self.nested(pending, '`', depth)?;
            }
            ('$', Dialect::Posix | Dialect::PowerShell) if self.peek(0) == Some('(') => {
                self.at += 1;
                pending.push('$');
                self.nested(pending, ')', depth)?;
            }
            ('$', Dialect::Posix | Dialect::PowerShell) if self.peek(0) == Some('{') => {
                // `${NAME}` stays one word, so its path reads as a variable.
                pending.push('$');
                while let Some(next) = self.peek(0) {
                    self.at += 1;
                    pending.word.text.push(next);
                    if next == '}' {
                        break;
                    }
                }
            }
            ('\'', Dialect::Posix | Dialect::PowerShell) => {
                pending.started = true;
                while let Some(next) = self.peek(0) {
                    self.at += 1;
                    if next == '\'' {
                        if self.dialect == Dialect::PowerShell && self.peek(0) == Some('\'') {
                            self.at += 1;
                        } else {
                            break;
                        }
                    }
                    pending.word.text.push(next);
                }
            }
            ('"', _) => {
                pending.started = true;
                self.double_quoted(pending, depth)?;
            }
            _ => pending.push(ch),
        }
        Ok(())
    }

    fn double_quoted(&mut self, pending: &mut Pending, depth: usize) -> Result<(), TooDeep> {
        while let Some(ch) = self.peek(0) {
            self.at += 1;
            match (ch, self.dialect) {
                ('"', Dialect::PowerShell) if self.peek(0) == Some('"') => {
                    self.at += 1;
                    pending.word.text.push('"');
                }
                ('"', _) => return Ok(()),
                ('\\', Dialect::Posix) if matches!(self.peek(0), Some('"' | '\\' | '$' | '`')) => {
                    if let Some(next) = self.peek(0) {
                        self.at += 1;
                        pending.word.text.push(next);
                    }
                }
                ('`', Dialect::PowerShell) => {
                    if let Some(next) = self.peek(0) {
                        self.at += 1;
                        pending.word.text.push(next);
                    }
                }
                ('`', Dialect::Posix) => {
                    pending.word.text.push('$');
                    self.nested(pending, '`', depth)?;
                }
                ('$', Dialect::Posix | Dialect::PowerShell) if self.peek(0) == Some('(') => {
                    self.at += 1;
                    pending.word.text.push('$');
                    self.nested(pending, ')', depth)?;
                }
                _ => pending.word.text.push(ch),
            }
        }
        Ok(())
    }
}

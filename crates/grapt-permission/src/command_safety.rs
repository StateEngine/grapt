//! Destructive shell command classification for the `maintainer` preset.
//!
//! `maintainer` runs commands unasked (decided 2026-10-07), except a command
//! that could destroy work or reach past the Session's workspace: those ask.
//! The classifier reads the permission resource a shell call is decided on,
//! including a terminal's `workdir:` and `stdin:` lines, and answers which
//! [`Danger`] a command could cause, or `None`.
//!
//! It never runs anything and never decides on its own: a configured or
//! approved `allow` still lifts its `ask` (see `preset`). It is a guard
//! against accidents, not a sandbox: an interpreter given code (`python -c`)
//! or a script file is not read.
//!
//! The wrapper unwrapping (`sudo`, `env`, shells' `-c`), the forced `rm`
//! flags, cmd's `del`/`rd` flags and PowerShell's forced deletes (Apache-2.0,
//! Copyright 2025 OpenAI). Modified by Grapt: a dependency-free lexer read under all three dialects
//! replaces tree-sitter, and asking also covers recursive deletes, Git
//! replaces tree-sitter, and asking also covers recursive deletes, Git
//! discards, disks, recursive ownership, outside writes and piped scripts.

mod git;
mod lexer;
mod prefix;
mod rules;
mod targets;
mod windows;

pub use prefix::{simple_commands, CommandReading};

use lexer::{Command, Dialect, Word};

/// Why a command asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Danger {
    /// A recursive, forced or mass delete.
    Delete,
    /// A Git command that discards commits, changes or remote history.
    Git,
    /// A disk, partition, boot or power command.
    Disk,
    /// A recursive change of permissions or ownership.
    Permissions,
    /// A write, move or delete outside the Session's workspace.
    OutsideWorkspace,
    /// A script fetched or produced at run time handed to an interpreter.
    PipedScript,
    /// Running as another, usually more privileged, user.
    Privilege,
    /// Too long or too nested to read, or deliberately encoded.
    Opaque,
    /// A command whose words come partly from input the check cannot read,
    /// as `xargs`, `parallel`, `fd -x` and `find -execdir` build them.
    Fed,
}

impl Danger {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Delete => "recursive or forced delete",
            Self::Git => "Git command that discards work",
            Self::Disk => "disk, boot or power command",
            Self::Permissions => "recursive permission or ownership change",
            Self::OutsideWorkspace => "write outside the workspace",
            Self::PipedScript => "generated script run by an interpreter",
            Self::Privilege => "command run as another user",
            Self::Opaque => "command too complex to check",
            Self::Fed => "command built from input it has not seen",
        }
    }
}

/// Resources longer than this are not read; they ask.
const MAX_RESOURCE_BYTES: usize = 1024 * 1024;
/// Interpreter and wrapper nesting checked before a command asks.
const MAX_DEPTH: usize = 8;
/// Labels a terminal resource puts before its metadata and input lines.
const LABELS: [&str; 6] = [
    "workdir: ",
    "shell: ",
    "login: ",
    "terminal: ",
    "stdin: ",
    "stdin line: ",
];
/// Labels whose text is also read on its own: terminal input arrives in
/// pieces, so a piece is read alone and joined to the unfinished line before it.
const SEGMENTS: [&str; 2] = ["stdin: ", "stdin line: "];
/// A resource line saying earlier terminal input is unknown: too long to
/// keep, or delivered uncertainly.
pub const UNREAD_INPUT: &str = "stdin unread";

/// The unfinished end of input typed into a terminal: what follows the last
/// line end that a POSIX shell, PowerShell and cmd all read as ending a
/// command line. Text continued by `\`, `` ` `` or `^`, or by an open quote,
/// stays unfinished, so the input completing it is read joined to it.
pub fn unfinished_line(input: &str) -> &str {
    &input[lexer::unfinished_start(input)..]
}

/// Classify a shell permission resource. `workspace` is the Session's
/// directory; without one, every absolute write target is outside.
pub fn classify(resource: &str, workspace: Option<&str>) -> Option<Danger> {
    // Line-editing keys (backspace, Ctrl-U, escape sequences) can rewrite
    // what a terminal runs; only Enter, Tab, Ctrl-C and Ctrl-D are read.
    if opaque_resource(resource) {
        return Some(Danger::Opaque);
    }
    let mut analyzer = Analyzer {
        workspace: workspace.map(normalize),
        outside_cwd: false,
        reading: None,
    };
    let texts = analyzer.resource_texts(resource);
    let entered = analyzer.outside_cwd;
    texts.into_iter().find_map(|text| {
        analyzer.outside_cwd = entered;
        analyzer.text(&text, 0)
    })
}

fn opaque_resource(resource: &str) -> bool {
    resource.len() > MAX_RESOURCE_BYTES
        || resource.split('\n').any(|line| line == UNREAD_INPUT)
        || resource
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{3}' | '\u{4}'))
}

struct Analyzer {
    workspace: Option<String>,
    /// A `cd` or terminal directory left the workspace: relative paths are outside.
    outside_cwd: bool,
    reading: Option<CommandReading>,
}

impl Analyzer {
    fn resource_texts(&mut self, resource: &str) -> Vec<String> {
        let mut whole = String::with_capacity(resource.len());
        let mut segments = vec![String::new()];
        for line in resource.split('\n') {
            let mut line = line;
            for label in LABELS {
                if let Some(rest) = line.strip_prefix(label) {
                    // A forged `workdir:` line can only make reading stricter.
                    if label == "workdir: " && self.outside(rest) {
                        self.outside_cwd = true;
                    }
                    if SEGMENTS.contains(&label) {
                        segments.push(String::new());
                    }
                    line = rest;
                }
            }
            whole.push_str(line);
            whole.push('\n');
            if let Some(segment) = segments.last_mut() {
                segment.push_str(line);
                segment.push('\n');
            }
        }
        // Read whole, so continued lines join, and each segment alone, so an
        // unclosed quote in one cannot hide the next.
        std::iter::once(whole).chain(segments).collect()
    }

    fn text(&mut self, text: &str, depth: usize) -> Option<Danger> {
        if depth > MAX_DEPTH {
            return self.opaque();
        }
        if self
            .reading
            .as_mut()
            .is_some_and(|reading| !reading.visit())
        {
            return Some(Danger::Opaque);
        }
        let mut found = None;
        for dialect in [Dialect::Posix, Dialect::PowerShell, Dialect::Cmd] {
            let entered = self.outside_cwd;
            let Ok(commands) = lexer::lex(text, dialect) else {
                if self.reading.is_none() {
                    return Some(Danger::Opaque);
                }
                self.incomplete();
                found = Some(Danger::Opaque);
                continue;
            };
            for command in &commands {
                if self
                    .reading
                    .as_ref()
                    .is_some_and(|reading| reading.exhausted())
                {
                    return Some(Danger::Opaque);
                }
                if let Some(danger) = self.command(command, depth) {
                    if self.reading.is_none() && danger != Danger::Opaque {
                        return Some(danger);
                    }
                    if matches!(danger, Danger::Opaque | Danger::PipedScript) {
                        self.incomplete();
                    }
                    found = found.or(Some(danger));
                }
            }
            self.outside_cwd = entered;
        }
        found
    }

    fn opaque(&mut self) -> Option<Danger> {
        self.incomplete();
        Some(Danger::Opaque)
    }

    fn incomplete(&mut self) {
        if let Some(reading) = &mut self.reading {
            reading.incomplete();
        }
    }

    fn command(&mut self, command: &Command, depth: usize) -> Option<Danger> {
        let outside = command
            .writes
            .iter()
            .any(|target| self.outside_word(target));
        if outside && self.reading.is_none() {
            return Some(Danger::OutsideWorkspace);
        }
        // Redirected writes are consequences beyond an argv prefix. Known
        // prefix restrictions still apply; a prefix alone cannot grant them.
        if !command.writes.is_empty() {
            self.incomplete();
        }
        let danger = self.words(&command.words, command.piped, depth);
        if outside {
            Some(Danger::OutsideWorkspace)
        } else {
            danger
        }
    }

    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn words(&mut self, words: &[Word], piped: bool, depth: usize) -> Option<Danger> {
        if depth > MAX_DEPTH {
            return self.opaque();
        }
        let start = words
            .iter()
            .position(|word| !is_assignment(&word.text))
            .unwrap_or(words.len());
        // An assignment can install executable startup input (BASH_ENV,
        // loader hooks, and others) before the visible argv is evaluated.
        if start > 0 {
            self.incomplete();
        }
        let words = &words[start..];
        let (first, args) = words.split_first()?;
        // The destructive classifier deliberately keeps variables and globs
        // as text. Their runtime argv is unknown to a literal positive rule.
        // Quoted literals may conservatively ask as well across the dialects.
        if words
            .iter()
            .any(|word| word.dynamic || word.text.contains(['$', '%', '!', '*', '?', '[']))
        {
            self.incomplete();
        }
        if first.dynamic
            || (!matches!(first.text.as_str(), "[" | "!")
                && first.text.contains(['$', '%', '!', '*', '?', '[']))
            || windows::attached_command(&first.text)
        {
            if let Some(reading) = &mut self.reading {
                reading.record(words);
            }
            return self.opaque();
        }
        // cmd's leading `@` suppresses echo; it does not change the command.
        let name = program_name(first.text.trim_start_matches('@'));
        if name == "git" && git::arguments(args).is_err() {
            self.incomplete();
        }
        // These forms execute script text or control flow whose complete
        // runtime argv this classifier does not promise to enumerate.
        if matches!(
            name.as_str(),
            "source"
                | "."
                | "python"
                | "python2"
                | "python3"
                | "py"
                | "perl"
                | "ruby"
                | "node"
                | "php"
                | "lua"
                | "if"
                | "then"
                | "elif"
                | "else"
                | "fi"
                | "for"
                | "while"
                | "until"
                | "do"
                | "done"
                | "case"
                | "esac"
                | "function"
                | "select"
                | "foreach"
                | "switch"
                | "try"
                | "catch"
                | "finally"
                | "trap"
        ) {
            self.incomplete();
        }
        let wrapped = rules::wrapped(&name, args);
        if wrapped.is_some_and(|rest| {
            rest.is_none_or(|rest| !prefix::wrapper_options_known(&name, args, rest))
        }) {
            self.incomplete();
        }
        let transparent = wrapped.is_some()
            || matches!(
                name.as_str(),
                "env"
                    | "sh"
                    | "bash"
                    | "zsh"
                    | "dash"
                    | "ksh"
                    | "mksh"
                    | "ash"
                    | "fish"
                    | "csh"
                    | "tcsh"
                    | "pwsh"
                    | "powershell"
                    | "cmd"
                    | "eval"
                    | "watch"
                    | "xargs"
            );
        if let Some(reading) = &mut self.reading {
            if first.dynamic {
                reading.incomplete();
            }
            // A path, suffix or case-folded wrapper is an extra invocation,
            // not an implicit grant for a different literal program name.
            if !transparent || first.text != name {
                reading.record(words);
            }
        }
        let before = self.reading.as_ref().map(CommandReading::len);
        let result = match name.as_str() {
            "sudo" | "doas" | "su" | "pkexec" | "runas" | "gsudo" => Some(Danger::Privilege),
            "env" => self.env(args, piped, depth),
            "xargs" => self.fed(rules::xargs(args), depth),
            "parallel" => self.fed(rules::parallel(args), depth),
            "fd" | "fdfind" => match rules::fd_exec(args) {
                Some(template) => self.fed(Some(template), depth),
                None => None,
            },
            "find" => self.find(args, depth),
            "eval" if args.iter().any(|word| word.dynamic) => Some(Danger::PipedScript),
            "eval" | "watch" => {
                const WATCH: rules::Spec = rules::Spec {
                    flags: "bcdeghptwx",
                    valued: "nq",
                    long_flags: &[
                        "beep",
                        "color",
                        "no-color",
                        "differences",
                        "errexit",
                        "chgexit",
                        "precise",
                        "no-title",
                        "no-wrap",
                        "exec",
                    ],
                    long_valued: &["interval", "equexit"],
                };
                let spec = if name == "eval" {
                    rules::Spec::NONE
                } else {
                    WATCH
                };
                match rules::after_options(args, &spec) {
                    Some(script) => {
                        if name == "watch" && !prefix::wrapper_options_known(&name, args, script) {
                            self.incomplete();
                        }
                        self.text(&join(script), depth + 1)
                    }
                    None => Some(Danger::Opaque),
                }
            }
            "iex" | "invoke-expression" => Some(Danger::PipedScript),
            "source" | "." => args
                .first()
                .is_some_and(|word| word.dynamic)
                .then_some(Danger::PipedScript),
            "cd" | "chdir" | "pushd" | "set-location" | "sl" | "push-location" => {
                let target = args.iter().find(|word| !word.text.starts_with('-'));
                if target.is_none_or(|word| self.outside_word(word)) {
                    self.outside_cwd = true;
                }
                None
            }
            "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "ash" | "fish" | "csh" | "tcsh" => {
                self.shell(&name, args, piped, depth)
            }
            "pwsh" | "powershell" => self.powershell(args, piped, depth),
            "cmd" => self.cmd(args, piped, depth),
            // These Windows launch/control forms re-expand input or choose a
            // command boundary we do not model. Keep visible restrictions,
            // but never let a partial reading authorize the whole expression.
            "call" | "start" | "forfiles" => {
                self.incomplete();
                self.script(args, depth).or(Some(Danger::Opaque))
            }
            "if" | "for" if windows::control_flow(&name, args) => self.opaque(),
            "python" | "python2" | "python3" | "py" | "perl" | "ruby" | "node" | "php" | "lua" => {
                let script = args.iter().find(|word| !word.text.starts_with('-'));
                let stdin = script.is_none_or(|word| word.text == "-");
                (args.iter().any(|word| word.dynamic) || (piped && stdin))
                    .then_some(Danger::PipedScript)
            }
            _ => {
                if let Some(rest) = wrapped {
                    match rest {
                        Some(rest) => self.words(rest, piped, depth + 1),
                        None => Some(Danger::Opaque),
                    }
                } else if let Some(danger) = rules::danger(&name, args, piped) {
                    Some(danger)
                } else {
                    targets::write_targets(&name, args)
                        .iter()
                        .any(|target| self.outside_word(target))
                        .then_some(Danger::OutsideWorkspace)
                }
            }
        };
        if matches!(result, Some(Danger::Opaque | Danger::PipedScript))
            || (transparent
                && before.is_some()
                && before == self.reading.as_ref().map(CommandReading::len))
        {
            self.incomplete();
        }
        result
    }

    /// GNU `env`: assignments and known options, then the command. `-S`
    /// splits a string into the command, which is read as a script. An
    /// option this does not know might take a value, so it asks.
    fn env(&mut self, args: &[Word], piped: bool, depth: usize) -> Option<Danger> {
        let mut index = 0;
        while let Some(word) = args.get(index) {
            let text = word.text.as_str();
            if is_assignment(text) {
                self.incomplete();
            }
            if text.starts_with('-') && !prefix::env_option_known(text) {
                self.incomplete();
            }
            index += 1;
            if text == "--" {
                break;
            }
            if is_assignment(text) || matches!(text, "-" | "-i" | "-0" | "-v") {
                continue;
            }
            let long = text.strip_prefix("--");
            let (name, value) = match long.map(|long| long.split_once('=')) {
                Some(Some((name, value))) => (Some(name), Some(value)),
                Some(None) => (long, None),
                None => (None, None),
            };
            if let Some(name) = name {
                match name {
                    "ignore-environment" | "null" | "debug" | "list-signal-handling"
                        if value.is_none() => {}
                    // Signal lists are optional and given only with `=`.
                    "block-signal" | "default-signal" | "ignore-signal" => {}
                    "split-string" => {
                        let script = value.or_else(|| args.get(index).map(|w| w.text.as_str()));
                        return script
                            .map_or(Some(Danger::Opaque), |script| self.text(script, depth + 1));
                    }
                    "unset" | "chdir" => {
                        let value = match value {
                            Some(value) => Some(value),
                            None => {
                                index += 1;
                                args.get(index - 1).map(|w| w.text.as_str())
                            }
                        };
                        let Some(value) = value else {
                            return Some(Danger::Opaque);
                        };
                        if name == "chdir" && self.outside(value) {
                            self.outside_cwd = true;
                        }
                    }
                    _ => return Some(Danger::Opaque),
                }
                continue;
            }
            let Some(letters) = text.strip_prefix('-') else {
                index -= 1;
                break;
            };
            for (at, letter) in letters.char_indices() {
                let rest = &letters[at + letter.len_utf8()..];
                match letter {
                    'i' | '0' | 'v' => continue,
                    'S' | 'u' | 'C' => {
                        let value = if rest.is_empty() {
                            index += 1;
                            match args.get(index - 1) {
                                Some(word) => word.text.as_str(),
                                None => return Some(Danger::Opaque),
                            }
                        } else {
                            rest
                        };
                        if letter == 'S' {
                            return self.text(value, depth + 1);
                        }
                        if letter == 'C' && self.outside(value) {
                            self.outside_cwd = true;
                        }
                    }
                    _ => return Some(Danger::Opaque),
                }
                break;
            }
        }
        self.words(&args[index.min(args.len())..], piped, depth + 1)
    }

    /// The one reading of a command run with input it has not seen:
    /// `xargs` appends or substitutes its input, and `parallel`, `fd -x` and
    /// `find -exec ... {}` substitute names. That input can become any word
    /// -- an option, a subcommand, the program (`echo reset | xargs -I s git s
    /// --hard` runs `git reset --hard`) -- so the argv is never fully known
    /// and the command always asks. A `template` whose own words are already
    /// dangerous gives that reason instead; `None` means even the template
    /// could not be told from the options.
    fn fed(&mut self, template: Option<&[Word]>, depth: usize) -> Option<Danger> {
        self.incomplete();
        template
            .and_then(|template| self.words(template, true, depth + 1))
            .filter(|danger| *danger != Danger::Opaque)
            .or(Some(Danger::Fed))
    }

    fn find(&mut self, args: &[Word], depth: usize) -> Option<Danger> {
        let mut index = 0;
        let mut found = None;
        while let Some(word) = args.get(index) {
            index += 1;
            match word.text.as_str() {
                "-delete" => {
                    if self.reading.is_none() {
                        return Some(Danger::Delete);
                    }
                    found = Some(Danger::Delete);
                }
                "-exec" | "-execdir" | "-ok" | "-okdir" => {
                    // `find` replaces `{}` with runtime input as well.
                    self.incomplete();
                    let end = args[index..]
                        .iter()
                        .position(|word| word.text == ";" || word.text == "+")
                        .map_or(args.len(), |at| index + at);
                    // Like `xargs`, `-exec` runs once per file found.
                    let template = &args[index..end];
                    let danger = if template.iter().any(|word| word.text.contains("{}")) {
                        self.fed(Some(template), depth)
                    } else {
                        self.words(template, true, depth + 1)
                    };
                    if let Some(danger) = danger {
                        if self.reading.is_none() {
                            return Some(danger);
                        }
                        found = found.or(Some(danger));
                    }
                    index = end;
                }
                _ => {}
            }
        }
        found
    }

    /// POSIX shells, fish and csh: `-c SCRIPT` is read; stdin or a generated
    /// file is a piped script. Options are checked against the shell's own:
    /// an unknown one might take a value (`bash +o posix -c ...`, `bash
    /// --rcfile x -c ...`), which would then be read as the script.
    fn shell(&mut self, name: &str, args: &[Word], piped: bool, depth: usize) -> Option<Danger> {
        // fish `-C`/`--init-command` runs a command too: not modelled, so it
        // is not listed and asks.
        let (flags, valued, long_flags, long_valued): (&str, &str, &[&str], &[&str]) = match name {
            "fish" => (
                "ilnPNvh",
                "dofp",
                &[
                    "login",
                    "interactive",
                    "no-execute",
                    "private",
                    "no-config",
                    "version",
                    "help",
                ],
                &[
                    "debug",
                    "debug-output",
                    "features",
                    "profile",
                    "profile-startup",
                ],
            ),
            "csh" | "tcsh" => ("bdefimnqtvxVX", "", &[], &[]),
            _ => (
                "abefhiklmnprtuvxBCDEHPT",
                "oO",
                &[
                    "login",
                    "noprofile",
                    "norc",
                    "posix",
                    "restricted",
                    "verbose",
                    "version",
                    "help",
                    "noediting",
                    "debugger",
                    "dump-strings",
                    "dump-po-strings",
                    "pretty-print",
                ],
                &["rcfile", "init-file"],
            ),
        };
        let (mut index, mut inline, mut stdin) = (0, false, false);
        while let Some(word) = args.get(index) {
            let text = word.text.as_str();
            index += 1;
            if text == "--" {
                break;
            }
            if text == "-" {
                stdin = true;
                break;
            }
            if let Some(long) = text.strip_prefix("--") {
                // A known option can still supply startup code or an output
                // file. Reading its command does not grant that consequence.
                let option = long.split_once('=').map_or(long, |(name, _)| name);
                if long_valued.contains(&option) {
                    self.incomplete();
                }
                match long.split_once('=') {
                    Some(("command", script)) => return self.text(script, depth + 1),
                    Some((name, _)) if long_valued.contains(&name) => {}
                    None if long == "command" => inline = true,
                    None if long_valued.contains(&long) => index += 1,
                    None if long_flags.contains(&long) => {}
                    _ => return Some(Danger::Opaque),
                }
                continue;
            }
            let Some(letters) = text.strip_prefix('-').or_else(|| text.strip_prefix('+')) else {
                index -= 1;
                break;
            };
            for (at, letter) in letters.char_indices() {
                match letter {
                    'c' => inline = true,
                    's' => stdin = true,
                    letter if flags.contains(letter) => {}
                    letter if valued.contains(letter) => {
                        // fish's short forms also accept output files.
                        if name == "fish" {
                            self.incomplete();
                        }
                        if letters[at + letter.len_utf8()..].is_empty() {
                            index += 1;
                        }
                        break;
                    }
                    _ => return Some(Danger::Opaque),
                }
            }
        }
        match args.get(index) {
            Some(script) if script.dynamic => Some(Danger::PipedScript),
            Some(script) if inline => self.text(&script.text, depth + 1),
            Some(_) if !stdin => None,
            _ => piped.then_some(Danger::PipedScript),
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn powershell(&mut self, args: &[Word], piped: bool, depth: usize) -> Option<Danger> {
        let mut index = 0;
        while let Some(word) = args.get(index) {
            let lower = word.text.to_ascii_lowercase();
            let Some(parameter) = lower.strip_prefix('-').or_else(|| lower.strip_prefix('/'))
            else {
                // Windows PowerShell treats a bare argument as a command.
                return self.script(&args[index..], depth);
            };
            let (key, inline) = match parameter.split_once(':') {
                Some((key, value)) => (key, Some(value)),
                None => (parameter, None),
            };
            if key == "e" || key == "ec" || (key.len() >= 2 && "encodedcommand".starts_with(key)) {
                return Some(Danger::Opaque);
            }
            if !key.is_empty() && "command".starts_with(key) {
                // `-Command:x y` runs `x y`: the inline value and every later word.
                let original = inline
                    .and(word.text.split_once(':'))
                    .map(|(_, value)| value);
                let mut script: Vec<Word> = original
                    .map(|value| Word {
                        text: value.to_owned(),
                        dynamic: word.dynamic,
                    })
                    .into_iter()
                    .collect();
                script.extend_from_slice(&args[index + 1..]);
                if script.len() == 1 && script[0].text == "-" {
                    return piped.then_some(Danger::PipedScript);
                }
                return self.script(&script, depth);
            }
            if !key.is_empty() && "file".starts_with(key) {
                let file = args.get(index + 1);
                return (file.is_some_and(|word| word.dynamic)
                    || (piped && file.is_none_or(|word| word.text == "-")))
                .then_some(Danger::PipedScript);
            }
            // A parameter may be abbreviated. Any prefix of a value-taking
            // name takes a value, so a value is never read as the command;
            // a parameter that names nothing known asks.
            const VALUED: &[&str] = &[
                "executionpolicy",
                "ep",
                "ex",
                "workingdirectory",
                "wd",
                "windowstyle",
                "w",
                "version",
                "v",
                "outputformat",
                "of",
                "inputformat",
                "if",
                "configurationname",
                "configurationfile",
                "settingsfile",
                "psconsolefile",
                "custompipename",
            ];
            const FLAGS: &[&str] = &[
                "noprofile",
                "nop",
                "noninteractive",
                "nologo",
                "noexit",
                "sta",
                "mta",
                "login",
                "l",
                "interactive",
                "noprofileloadtime",
                "help",
                "?",
            ];
            // `-` alone reads the script from stdin.
            if key.is_empty() {
                index += 1;
                continue;
            }
            let takes_value = VALUED
                .iter()
                .any(|name| *name == key || (key.len() >= 2 && name.starts_with(key)));
            if !prefix::powershell_option_known(key, VALUED) {
                self.incomplete();
            }
            if !takes_value
                && !FLAGS
                    .iter()
                    .any(|name| *name == key || (key.len() >= 3 && name.starts_with(key)))
            {
                return Some(Danger::Opaque);
            }
            if key == "wd" || (key.len() >= 2 && "workingdirectory".starts_with(key)) {
                let original = word.text.split_once(':').map(|(_, value)| value);
                let directory =
                    original.or_else(|| args.get(index + 1).map(|next| next.text.as_str()));
                if directory.is_none_or(|directory| self.outside(directory)) {
                    self.outside_cwd = true;
                }
            }
            index += if takes_value && inline.is_none() {
                2
            } else {
                1
            };
        }
        piped.then_some(Danger::PipedScript)
    }

    fn outside_word(&self, word: &Word) -> bool {
        word.dynamic || self.outside(&word.text)
    }

    /// Whether writing `path` could reach past the workspace.
    fn outside(&self, path: &str) -> bool {
        let path = path.trim();
        let lower = path.to_ascii_lowercase();
        if path.is_empty()
            || matches!(
                lower.as_str(),
                "-" | "/dev/null"
                    | "/dev/stdout"
                    | "/dev/stderr"
                    | "/dev/tty"
                    | "nul"
                    | "nul:"
                    | "$null"
                    | "con"
            )
            || lower.starts_with("/dev/fd/")
        {
            return false;
        }
        let relative = ["$pwd", "${pwd}"].iter().find_map(|prefix| {
            lower
                .strip_prefix(prefix)
                .filter(|rest| rest.is_empty() || rest.starts_with(['/', '\\']))
                .map(|rest| rest.trim_start_matches(['/', '\\']))
        });
        if let Some(rest) = relative {
            return self.outside_cwd || escapes(rest);
        }
        if path.starts_with(['~', '$', '%']) {
            return true;
        }
        if is_absolute(path) {
            let target = normalize(path);
            return !self.workspace.as_ref().is_some_and(|root| {
                target == *root
                    || target
                        .strip_prefix(root.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            });
        }
        self.outside_cwd || escapes(path)
    }
}

/// The lookup key of a program word: its file name, lowercased, without a
/// Windows drive or executable suffix.
fn program_name(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    let name = lower.rsplit(['/', '\\']).next().unwrap_or_default();
    let name = match name.as_bytes() {
        [drive, b':', ..] if drive.is_ascii_alphabetic() => &name[2..],
        _ => name,
    };
    [".exe", ".cmd", ".bat", ".com"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(name)
        .to_owned()
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.starts_with(|c: char| c.is_ascii_digit())
    })
}

fn join(words: &[Word]) -> String {
    words
        .iter()
        .map(|word| word.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with(['/', '\\'])
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// A relative path whose `..` segments climb above its starting directory.
fn escapes(path: &str) -> bool {
    let mut depth: usize = 0;
    for segment in path.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => match depth.checked_sub(1) {
                Some(up) => depth = up,
                None => return true,
            },
            _ => depth += 1,
        }
    }
    false
}

/// Forward slashes, no `.`/`..`/empty segments, and a lowercase drive path,
/// so a workspace and a target compare as text.
fn normalize(path: &str) -> String {
    let windows = cfg!(windows) || path.starts_with(r"\\") || path.as_bytes().get(1) == Some(&b':');
    let path = path.replace('\\', "/");
    let path = if windows {
        grapt_types::path::windows_comparison_key(&path)
    } else {
        path
    };
    let drive = path.as_bytes().get(1) == Some(&b':');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    let joined = segments.join("/");
    if drive {
        joined.to_ascii_lowercase()
    } else {
        format!("/{joined}")
    }
}

#[cfg(test)]
#[path = "command_safety/tests.rs"]
mod tests;

/// One literal cmd command, without expansion, redirection or sibling effects.
/// This is used only to prepare an explicitly approved native executable;
/// compound/ambiguous source must retain its ordinary shell execution path.
pub fn literal_windows_argv(source: &str) -> Option<Vec<String>> {
    if source.len() > 65536
        || source.contains(['^', '%', '!', '\r', '\n'])
        || source.contains("\\\"")
        || !source.matches('"').count().is_multiple_of(2)
    {
        return None;
    }
    let mut quoted = false;
    for character in source.chars() {
        if character == '"' {
            quoted = !quoted;
        } else if character.is_control() && character != '\t'
            || !quoted && "&|<>(){};".contains(character)
        {
            return None;
        }
    }
    let commands = lexer::lex(source, Dialect::Cmd).ok()?;
    let [command] = commands.as_slice() else {
        return None;
    };
    if command.piped
        || !command.writes.is_empty()
        || command.words.is_empty()
        || command.words.iter().any(|word| word.dynamic)
    {
        return None;
    }
    Some(command.words.iter().map(|word| word.text.clone()).collect())
}

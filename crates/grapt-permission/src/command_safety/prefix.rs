//! Prefix-policy input from the classifier's existing lexer and wrapper walker.
use super::*;

const MAX_COMMANDS: usize = 256;
const MAX_WORDS: usize = 256;
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;
const MAX_READINGS: usize = 128;

/// Known argv readings. An incomplete reading can support Ask or Deny on
/// known commands, but must never authorize the expression with Allow.
#[derive(Debug)]
pub struct CommandReading {
    commands: Vec<Vec<String>>,
    complete: bool,
    exhausted: bool,
    bytes: usize,
    remaining: usize,
}

impl CommandReading {
    /// Original program spelling and argument text after shell quote removal.
    pub fn commands(&self) -> &[Vec<String>] {
        &self.commands
    }

    /// Every interpretation was read completely within the declared bounds.
    pub fn is_complete(&self) -> bool {
        self.complete && !self.exhausted && !self.commands.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.commands.len()
    }

    pub(super) fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub(super) fn incomplete(&mut self) {
        self.complete = false;
    }

    pub(super) fn visit(&mut self) -> bool {
        match self.remaining.checked_sub(1) {
            Some(remaining) => {
                self.remaining = remaining;
                true
            }
            None => {
                self.exhausted = true;
                false
            }
        }
    }

    pub(super) fn record(&mut self, words: &[Word]) {
        let bytes = words
            .iter()
            .fold(0usize, |total, word| total.saturating_add(word.text.len()));
        if self.commands.len() >= MAX_COMMANDS
            || words.len() > MAX_WORDS
            || bytes > MAX_CAPTURE_BYTES.saturating_sub(self.bytes)
        {
            self.exhausted = true;
            return;
        }
        self.complete &= !words.iter().any(|word| word.dynamic);
        self.bytes += bytes;
        self.commands
            .push(words.iter().map(|word| word.text.clone()).collect());
    }
}

/// Read every simple command under all three classifier dialects, unwrapping
/// the same transparent wrappers. Dynamic or unread script input cannot grant.
/// The result is bounded to 256 commands, 256 words per command, 4 MiB of
/// captured text and 128 recursive text readings, within the classifier's
/// existing resource and nesting bounds.
pub fn simple_commands(resource: &str) -> CommandReading {
    let mut analyzer = Analyzer {
        workspace: None,
        outside_cwd: false,
        reading: Some(CommandReading {
            commands: Vec::new(),
            complete: true,
            exhausted: false,
            bytes: 0,
            remaining: MAX_READINGS,
        }),
    };
    if opaque_resource(resource) {
        analyzer.incomplete();
    } else {
        // Resource text cannot establish metadata provenance. Read every
        // labelled line conservatively; only the prepared terminal target can
        // supply its command separately at the product boundary.
        if resource
            .lines()
            .any(|line| LABELS.iter().any(|label| line.starts_with(label)))
        {
            analyzer.incomplete();
        }
        for text in analyzer.resource_texts(resource) {
            let _ = analyzer.text(&text, 0);
        }
    }
    analyzer.reading.take().unwrap_or_else(|| CommandReading {
        commands: Vec::new(),
        complete: false,
        exhausted: true,
        bytes: 0,
        remaining: 0,
    })
}

/// Positive authorization accepts only the wrapper option forms whose argv
/// forwarding the existing walker models. Other forms still expose known
/// restrictions, but cannot supply a complete positive reading.
pub(super) fn wrapper_options_known(name: &str, args: &[Word], rest: &[Word]) -> bool {
    // The lockfile is an additional write, not argv forwarding.
    if name == "flock" {
        return false;
    }
    let consumed = &args[..args.len().saturating_sub(rest.len())];
    let valued = match name {
        "nice" => "n",
        "ionice" => "cn",
        "stdbuf" => "ioe",
        "timeout" => "sk",
        "flock" => "wE",
        "watch" => "n",
        _ => "",
    };
    consumed.iter().all(|word| {
        let text = word.text.as_str();
        !text.starts_with('-')
            || text == "--"
            || text
                .strip_prefix('-')
                .and_then(|flags| flags.chars().next())
                .is_some_and(|flag| valued.contains(flag))
    })
}

/// Classification may consume abbreviations, while a grant keeps exact
/// modeled forms and excludes supplied startup configuration.
pub(super) fn powershell_option_known(key: &str, valued: &[&str]) -> bool {
    (valued.contains(&key)
        || matches!(
            key,
            "noprofile"
                | "nop"
                | "nologo"
                | "nol"
                | "noninteractive"
                | "noni"
                | "sta"
                | "mta"
                | "login"
                | "l"
                | "noexit"
                | "noe"
        ))
        && !matches!(
            key,
            "configurationname" | "configurationfile" | "settingsfile" | "psconsolefile"
        )
}

pub(super) fn env_option_known(text: &str) -> bool {
    matches!(
        text,
        "--" | "-i" | "--ignore-environment" | "-u" | "--unset" | "-C" | "--chdir"
    ) || text.starts_with("--unset=")
        || text.starts_with("--chdir=")
        || (text.starts_with("-u") && !text.starts_with("--"))
        || (text.starts_with("-C") && !text.starts_with("--"))
}

#[cfg(test)]
#[path = "prefix_tests.rs"]
mod tests;

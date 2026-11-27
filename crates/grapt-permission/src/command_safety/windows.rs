//! Windows shell switches whose command text needs another lexer pass.
use super::{join, Analyzer, Danger, Word};

impl Analyzer {
    pub(super) fn script(&mut self, words: &[Word], depth: usize) -> Option<Danger> {
        if words.iter().any(|word| word.dynamic) {
            return Some(Danger::PipedScript);
        }
        self.text(&join(words), depth + 1)
    }

    pub(super) fn cmd(&mut self, args: &[Word], piped: bool, depth: usize) -> Option<Danger> {
        for (index, word) in args.iter().enumerate() {
            let mut rest = word.text.as_str();
            if !rest.starts_with('/') {
                return self.script(&args[index..], depth);
            }
            while let Some(switch) = rest.strip_prefix('/') {
                let Some(letter) = switch.chars().next() else {
                    return self.opaque();
                };
                let tail = &switch[letter.len_utf8()..];
                if matches!(letter.to_ascii_lowercase(), 'c' | 'k' | 'r') {
                    // /cCOMMAND and /d/cCOMMAND are accepted without spaces.
                    let mut script = Vec::new();
                    if !tail.is_empty() {
                        script.push(Word {
                            text: tail.to_owned(),
                            dynamic: word.dynamic,
                        });
                    }
                    script.extend_from_slice(&args[index + 1..]);
                    return self.script(&script, depth);
                }
                let end = switch.find('/').unwrap_or(switch.len());
                let option = switch[..end].to_ascii_lowercase();
                if !matches!(
                    option.as_str(),
                    "d" | "s"
                        | "q"
                        | "a"
                        | "u"
                        | "e:on"
                        | "e:off"
                        | "f:on"
                        | "f:off"
                        | "v:on"
                        | "v:off"
                ) {
                    return self.opaque();
                }
                // Enabled expansion and startup processing are not literal argv.
                if matches!(option.as_str(), "v:on" | "e:on") {
                    self.incomplete();
                }
                rest = &switch[end..];
            }
        }
        piped.then_some(Danger::PipedScript)
    }
}

/// cmd conditions and loops choose a command after runtime-only input.
/// POSIX control words retain their existing lexer traversal.
pub(super) fn control_flow(name: &str, args: &[Word]) -> bool {
    match name {
        "if" => args.iter().any(|word| {
            matches!(
                word.text.to_ascii_lowercase().as_str(),
                "exist"
                    | "errorlevel"
                    | "defined"
                    | "/i"
                    | "not"
                    | "equ"
                    | "neq"
                    | "lss"
                    | "leq"
                    | "gtr"
                    | "geq"
            ) || ["exist", "errorlevel", "defined", "cmdextversion", "not"]
                .iter()
                .any(|keyword| word.text.to_ascii_lowercase().starts_with(keyword))
                || word.text.contains("==")
        }),
        "for" => args
            .first()
            .is_some_and(|word| word.text.starts_with(['/', '%'])),
        _ => false,
    }
}

/// cmd accepts punctuation next to internal command names without a space.
/// The general path/argv lexer cannot certify that boundary as a literal path.
pub(super) fn attached_command(word: &str) -> bool {
    let lower = word.trim_start_matches('@').to_ascii_lowercase();
    if lower.is_empty() && word.starts_with('@') {
        return true;
    }
    [
        "cmd", "cmd.exe", "call", "start", "if", "for", "rd", "rmdir", "del", "erase",
    ]
    .iter()
    .any(|name| {
        lower.strip_prefix(name).is_some_and(|tail| {
            !matches!(tail, ".exe" | ".cmd" | ".bat" | ".com")
                && tail.starts_with(['/', '.', ':', '=', ',', ';'])
        })
    })
}

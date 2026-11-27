//! Which files a command writes, moves or deletes, read through its option
//! grammar so attached values (`-t/etc`, `--output=/etc/x`), clusters
//! (`-fsSLo /etc/x`) and cmd switches (`/s`, not the path `/etc`) land where
//! the command itself would put them.

use super::lexer::Word;

/// The option grammar of a POSIX-style command.
#[derive(Clone, Copy)]
struct Grammar {
    /// Short letters that take a value, attached (`-t/x`) or as the next word.
    valued: &'static str,
    /// Short letters whose value can only be attached (`sed -i.bak`).
    attached: &'static str,
    /// Long options that take the next word when not written `--name=value`.
    long: &'static [&'static str],
}

const PLAIN: Grammar = Grammar {
    valued: "",
    attached: "",
    long: &[],
};

/// Arguments after option parsing. Keys are short letters or long names,
/// lowercased for PowerShell parameters.
#[derive(Default)]
struct Parsed {
    positional: Vec<Word>,
    values: Vec<(String, Word)>,
    flags: Vec<String>,
}

impl Parsed {
    fn value(&self, keys: &[&str]) -> Vec<Word> {
        self.values
            .iter()
            .filter(|(key, _)| keys.contains(&key.as_str()))
            .map(|(_, word)| word.clone())
            .collect()
    }

    fn flag(&self, keys: &[&str]) -> bool {
        self.flags.iter().any(|flag| keys.contains(&flag.as_str()))
            || self
                .values
                .iter()
                .any(|(key, _)| keys.contains(&key.as_str()))
    }
}

fn part(word: &Word, text: &str) -> Word {
    Word {
        text: text.to_owned(),
        dynamic: word.dynamic,
    }
}

fn posix(args: &[Word], grammar: Grammar) -> Parsed {
    let mut parsed = Parsed::default();
    let mut options = true;
    let mut index = 0;
    while let Some(word) = args.get(index) {
        index += 1;
        let text = word.text.as_str();
        if text == "--" && options {
            options = false;
            continue;
        }
        if !options || text == "-" || !text.starts_with('-') {
            parsed.positional.push(word.clone());
            continue;
        }
        if let Some(long) = text.strip_prefix("--") {
            match long.split_once('=') {
                Some((key, value)) => parsed.values.push((key.to_owned(), part(word, value))),
                None if grammar.long.contains(&long) => {
                    if let Some(next) = args.get(index) {
                        parsed.values.push((long.to_owned(), next.clone()));
                        index += 1;
                    }
                }
                None => parsed.flags.push(long.to_owned()),
            }
            continue;
        }
        let cluster = &text[1..];
        for (at, letter) in cluster.char_indices() {
            let rest = &cluster[at + letter.len_utf8()..];
            if grammar.valued.contains(letter) {
                let value = if rest.is_empty() {
                    index += 1;
                    args.get(index - 1).cloned()
                } else {
                    Some(part(word, rest))
                };
                parsed
                    .values
                    .extend(value.map(|value| (letter.to_string(), value)));
                break;
            }
            if grammar.attached.contains(letter) {
                parsed.values.push((letter.to_string(), part(word, rest)));
                break;
            }
            parsed.flags.push(letter.to_string());
        }
    }
    parsed
}

/// PowerShell parameters: `-Name value` or `-Name:value`, except switches.
/// A parameter may be any unambiguous prefix, so a prefix of a switch is a
/// switch and its following word stays positional.
fn powershell(args: &[Word], switches: &[&str]) -> Parsed {
    let mut parsed = Parsed::default();
    let mut index = 0;
    while let Some(word) = args.get(index) {
        index += 1;
        let lower = word.text.to_ascii_lowercase();
        let Some(parameter) = lower
            .strip_prefix('-')
            .filter(|name| name.starts_with(|c: char| c.is_ascii_alphabetic()))
        else {
            parsed.positional.push(word.clone());
            continue;
        };
        if let Some((name, value)) = parameter.split_once(':') {
            let at = word.text.len() - value.len();
            parsed
                .values
                .push((name.to_owned(), part(word, &word.text[at..])));
        } else if switches.iter().any(|switch| switch.starts_with(parameter)) {
            parsed.flags.push(parameter.to_owned());
        } else if let Some(next) = args.get(index) {
            parsed.values.push((parameter.to_owned(), next.clone()));
            index += 1;
        }
    }
    parsed
}

/// PowerShell parameters whose values are not paths.
const PS_DATA: [&str; 11] = [
    "value",
    "encoding",
    "inputobject",
    "itemtype",
    "filter",
    "include",
    "exclude",
    "width",
    "stream",
    "credential",
    "delimiter",
];
const PS_SWITCHES: [&str; 10] = [
    "force",
    "append",
    "noclobber",
    "nonewline",
    "passthru",
    "whatif",
    "confirm",
    "recurse",
    "container",
    "asbytestream",
];

/// Every word a PowerShell cmdlet could treat as a path: positionals and
/// the values of parameters that are not data.
fn ps_paths(parsed: &Parsed) -> Vec<Word> {
    let mut paths = parsed.positional.clone();
    paths.extend(
        parsed
            .values
            .iter()
            .filter(|(key, _)| !PS_DATA.iter().any(|data| data.starts_with(key.as_str())))
            .map(|(_, word)| word.clone()),
    );
    paths
}

/// The switches cmd's built-in `name` accepts; any other `/word` is a path.
fn cmd_switch(name: &str, text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let Some(switch) = lower.strip_prefix('/') else {
        return false;
    };
    match name {
        "del" | "erase" => matches!(switch, "p" | "f" | "s" | "q") || switch.starts_with("a"),
        "rd" | "rmdir" => matches!(switch, "s" | "q"),
        "copy" => matches!(switch, "a" | "b" | "v" | "n" | "y" | "-y" | "z" | "l" | "d"),
        "move" => matches!(switch, "y" | "-y"),
        "mklink" => matches!(switch, "d" | "h" | "j"),
        // Their many switches are short words; a second `/` makes a path.
        "xcopy" | "robocopy" => {
            !switch.contains('/') && switch.split(':').next().is_some_and(|key| key.len() <= 8)
        }
        _ => false,
    }
}

fn last(words: Vec<Word>) -> Vec<Word> {
    words.into_iter().last().into_iter().collect()
}

/// `host:path` names another machine; `C:path` is a local drive.
fn local(word: &Word) -> bool {
    match word.text.split_once(':') {
        Some((host, _)) => host.len() == 1 || host.contains(['/', '\\']),
        None => true,
    }
}

/// The files `name` writes, moves or deletes, as far as its arguments say.
/// Names that are both POSIX tools and PowerShell or cmd aliases are read
/// under every grammar they could have.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn write_targets(name: &str, written: &[Word]) -> Vec<Word> {
    let args: Vec<Word> = written
        .iter()
        .filter(|word| !cmd_switch(name, &word.text))
        .cloned()
        .collect();
    let args = args.as_slice();
    let ps = || powershell(args, &PS_SWITCHES);
    let mut targets = match name {
        "tee" | "rm" | "unlink" | "rmdir" | "md" | "del" | "erase" | "rd" | "ren" | "rename"
        | "mklink" => posix(args, PLAIN).positional,
        "touch" => {
            posix(
                args,
                Grammar {
                    valued: "drt",
                    long: &["date", "reference"],
                    ..PLAIN
                },
            )
            .positional
        }
        "mkdir" => {
            posix(
                args,
                Grammar {
                    valued: "m",
                    long: &["mode", "context"],
                    ..PLAIN
                },
            )
            .positional
        }
        "truncate" => {
            posix(
                args,
                Grammar {
                    valued: "rs",
                    long: &["reference", "size"],
                    ..PLAIN
                },
            )
            .positional
        }
        "shred" => {
            posix(
                args,
                Grammar {
                    valued: "ns",
                    long: &["iterations", "size", "random-source"],
                    ..PLAIN
                },
            )
            .positional
        }
        "mv" | "move" | "cp" | "copy" | "ln" | "install" => {
            let parsed = posix(
                args,
                Grammar {
                    valued: "tSgmo",
                    long: &[
                        "target-directory",
                        "suffix",
                        "backup",
                        "group",
                        "mode",
                        "owner",
                    ],
                    ..PLAIN
                },
            );
            let mut targets = parsed.value(&["t", "target-directory"]);
            if matches!(name, "mv" | "move") {
                // A move removes its sources too.
                targets.extend(parsed.positional);
            } else if targets.is_empty() {
                targets = last(parsed.positional);
            }
            targets
        }
        "xcopy" | "robocopy" => {
            let parsed = posix(args, PLAIN);
            let mut targets: Vec<Word> = parsed.positional.get(1).cloned().into_iter().collect();
            // `/LOG:file` and its variants write a log wherever they name.
            targets.extend(written.iter().filter_map(|word| {
                let lower = word.text.to_ascii_lowercase();
                let at = lower.find(':')?;
                lower[..at]
                    .contains("log")
                    .then(|| part(word, &word.text[at + 1..]))
            }));
            targets
        }
        "rsync" | "scp" => {
            let parsed = posix(
                args,
                Grammar {
                    valued: "eBfTcFiJlPoS",
                    long: &["rsh", "filter", "temp-dir", "partial-dir", "log-file"],
                    ..PLAIN
                },
            );
            let mut targets = parsed.value(&["log-file", "partial-dir", "temp-dir", "T"]);
            targets.extend(last(parsed.positional).into_iter().filter(local));
            targets
        }
        "sed" => {
            let parsed = posix(
                args,
                Grammar {
                    valued: "efl",
                    attached: "i",
                    long: &["expression", "file", "line-length"],
                },
            );
            if !parsed.flag(&["i", "in-place"]) {
                return Vec::new();
            }
            let scripted = parsed.flag(&["e", "f", "expression", "file"]);
            parsed
                .positional
                .into_iter()
                .skip(usize::from(!scripted))
                .collect()
        }
        "chmod" | "chown" | "chgrp" => {
            let parsed = posix(
                args,
                Grammar {
                    long: &["reference"],
                    ..PLAIN
                },
            );
            let skip = usize::from(!parsed.flag(&["reference"]));
            parsed.positional.into_iter().skip(skip).collect()
        }
        "curl" => posix(
            args,
            Grammar {
                valued: "AbcCdDeEFHKmorTuUwxXyYzQPt",
                long: &[
                    "output",
                    "output-dir",
                    "cookie-jar",
                    "dump-header",
                    "trace",
                    "trace-ascii",
                    "stderr",
                    "libcurl",
                    "etag-save",
                    "header",
                    "data",
                    "user-agent",
                    "url",
                ],
                ..PLAIN
            },
        )
        .value(&[
            "o",
            "c",
            "D",
            "output",
            "output-dir",
            "cookie-jar",
            "dump-header",
            "trace",
            "trace-ascii",
            "stderr",
            "libcurl",
            "etag-save",
        ]),
        "wget" => posix(
            args,
            Grammar {
                valued: "OoaPeiBTtwUQYlDAR",
                long: &[
                    "output-document",
                    "output-file",
                    "append-output",
                    "directory-prefix",
                    "user-agent",
                    "header",
                ],
                ..PLAIN
            },
        )
        .value(&[
            "O",
            "o",
            "a",
            "P",
            "output-document",
            "output-file",
            "append-output",
            "directory-prefix",
        ]),
        "tar" => {
            // Old-style `tar xfC archive dir` bundles options without a dash,
            // and each letter that takes a value takes the next word in turn.
            const TAR_VALUED: &str = "fCbHgKLNTVX";
            let mut args = args.to_vec();
            if let Some(first) = args.first().cloned().filter(|first| {
                !first.text.starts_with('-') && first.text.chars().all(|c| c.is_ascii_alphabetic())
            }) {
                let mut rest = args.split_off(1).into_iter();
                let mut letters = Vec::new();
                for letter in first.text.chars() {
                    letters.push(part(&first, &format!("-{letter}")));
                    if TAR_VALUED.contains(letter) {
                        letters.extend(rest.next());
                    }
                }
                letters.extend(rest);
                args = letters;
            }
            let parsed = posix(
                &args,
                Grammar {
                    valued: TAR_VALUED,
                    long: &["file", "directory", "exclude", "files-from"],
                    ..PLAIN
                },
            );
            let mut targets = parsed.value(&["C", "directory"]);
            if parsed.flag(&["c", "r", "u", "create", "append", "update"]) {
                targets.extend(parsed.value(&["f", "file"]));
            }
            targets
        }
        "unzip" => posix(
            args,
            Grammar {
                valued: "d",
                ..PLAIN
            },
        )
        .value(&["d"]),
        "out-file" | "set-content" | "add-content" | "ac" | "new-item" | "ni" | "clear-content"
        | "clc" | "remove-item" | "ri" | "rename-item" | "rni" | "move-item" | "mi"
        | "export-csv" | "expand-archive" => ps_paths(&ps()),
        "copy-item" | "cpi" => {
            let parsed = ps();
            let mut targets: Vec<Word> = parsed.positional.iter().skip(1).cloned().collect();
            targets.extend(
                parsed
                    .values
                    .iter()
                    .filter(|(key, _)| key.len() >= 2 && "destination".starts_with(key.as_str()))
                    .map(|(_, word)| word.clone()),
            );
            targets
        }
        "invoke-webrequest" | "iwr" | "invoke-restmethod" | "irm" => ps()
            .values
            .into_iter()
            .filter(|(key, _)| key.len() >= 2 && "outfile".starts_with(key.as_str()))
            .map(|(_, word)| word)
            .collect(),
        _ => Vec::new(),
    };
    // PowerShell resolves these names to cmdlets with their own parameters.
    let cmdlet = match name {
        "rm" | "del" | "erase" | "rd" | "rmdir" | "ren" | "md" | "mkdir" => "remove-item",
        "cp" | "copy" => "copy-item",
        "curl" | "wget" => "invoke-webrequest",
        _ => return targets,
    };
    targets.extend(write_targets(cmdlet, args));
    targets
}

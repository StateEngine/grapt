//! Which simple commands are destructive, and which files they write.
//! Program names arrive lowercased without a path or executable suffix;
//! arguments arrive as the lexer produced them, case intact.
//!
//! The forced `rm`, cmd `del`/`rd` and PowerShell forced-delete rules
//! follow an Apache-2.0 reference (Copyright 2025 OpenAI); see `command_safety`.
//! Modified by Grapt.

use super::lexer::Word;
use super::Danger;

/// The options a wrapper is known to take. Whatever is not here -- an
/// unknown letter or long option, an abbreviation, a missing value -- leaves
/// the boundary between options and the wrapped command unknown, and a check
/// that guessed it could read an option's value as the command.
pub(super) struct Spec {
    /// Letters that stand alone.
    pub flags: &'static str,
    /// Letters that take a value, attached or as the next word.
    pub valued: &'static str,
    pub long_flags: &'static [&'static str],
    /// Long options that take a value, after `=` or as the next word.
    pub long_valued: &'static [&'static str],
}

impl Spec {
    pub const NONE: Self = Self::short("", "");

    pub const fn short(flags: &'static str, valued: &'static str) -> Self {
        Self {
            flags,
            valued,
            long_flags: &[],
            long_valued: &[],
        }
    }
}

/// The words after leading options, or `None` when an option is not one
/// `spec` knows.
pub(super) fn after_options<'a>(args: &'a [Word], spec: &Spec) -> Option<&'a [Word]> {
    let mut index = 0;
    while let Some(word) = args.get(index) {
        let text = word.text.as_str();
        if text == "--" {
            return Some(&args[index + 1..]);
        }
        if !text.starts_with('-') || text == "-" {
            break;
        }
        index += 1;
        if let Some(long) = text.strip_prefix("--") {
            match long.split_once('=') {
                Some((name, _)) if spec.long_valued.contains(&name) => {}
                None if spec.long_valued.contains(&long) => {
                    args.get(index)?;
                    index += 1;
                }
                None if spec.long_flags.contains(&long) => {}
                _ => return None,
            }
            continue;
        }
        for (at, letter) in text.char_indices().skip(1) {
            if spec.flags.contains(letter) {
                continue;
            }
            if !spec.valued.contains(letter) {
                return None;
            }
            if text[at + letter.len_utf8()..].is_empty() {
                args.get(index)?;
                index += 1;
            }
            break;
        }
    }
    Some(&args[index..])
}

/// The command `xargs` runs with its input, or `None` when an option is not
/// one of xargs' own: an unknown option might take a value, and that value
/// -- a file named `grep`, say -- would then look like the command. Its
/// input always reaches the command's words, so this only names the
/// template whose own danger is reported first.
pub(super) fn xargs(args: &[Word]) -> Option<&[Word]> {
    const XARGS: Spec = Spec {
        // `-i`, `-e` and `-l` take a value only attached; one attached reads
        // as an unknown letter and leaves the command unknown.
        flags: "0prtxoiel",
        valued: "IJnPLsdEaRS",
        long_flags: &[
            "null",
            "no-run-if-empty",
            "verbose",
            "interactive",
            "exit",
            "show-limits",
            "open-tty",
            "replace",
            "eof",
            "max-lines",
        ],
        long_valued: &[
            "arg-file",
            "delimiter",
            "max-args",
            "max-procs",
            "max-chars",
            "process-slot-var",
        ],
    };
    after_options(args, &XARGS)
}

/// The command GNU `parallel` runs, before its `:::` input sources, or
/// `None` when options precede it: parallel has too many value options to
/// tell a value from the command.
pub(super) fn parallel(args: &[Word]) -> Option<&[Word]> {
    if args.first()?.text.starts_with('-') {
        return None;
    }
    let end = args
        .iter()
        .position(|word| word.text.starts_with(":::"))
        .unwrap_or(args.len());
    Some(&args[..end])
}

/// The command `fd -x`/`--exec` or `-X`/`--exec-batch` runs, if any.
pub(super) fn fd_exec(args: &[Word]) -> Option<&[Word]> {
    let at = args
        .iter()
        .position(|word| matches!(word.text.as_str(), "-x" | "--exec" | "-X" | "--exec-batch"))?;
    let rest = &args[at + 1..];
    let end = rest
        .iter()
        .position(|word| word.text == ";")
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The command a wrapper runs, when `name` only wraps one: `Some(None)`
/// when its options leave that command unknown.
pub(super) fn wrapped<'a>(name: &str, args: &'a [Word]) -> Option<Option<&'a [Word]>> {
    let skip_one = |rest: &'a [Word]| rest.get(1..).unwrap_or_default();
    let spec = match name {
        "nohup" | "builtin" | "then" | "do" | "else" | "elif" | "if" | "while" | "until" | "!"
        | "busybox" => Spec::NONE,
        "time" => Spec {
            flags: "apvq",
            valued: "of",
            long_flags: &["append", "verbose", "portability", "quiet"],
            long_valued: &["output", "format"],
        },
        "command" => Spec::short("pvV", ""),
        "exec" => Spec::short("cl", "a"),
        "setsid" => Spec {
            flags: "cfw",
            valued: "",
            long_flags: &["ctty", "fork", "wait"],
            long_valued: &[],
        },
        "unbuffer" => Spec::short("p", ""),
        "chronic" => Spec::short("ev", ""),
        "caffeinate" => Spec::short("dimsu", "tw"),
        "nice" => Spec {
            flags: "",
            valued: "n",
            long_flags: &[],
            long_valued: &["adjustment"],
        },
        "ionice" => Spec {
            flags: "t",
            valued: "cnpPu",
            long_flags: &["ignore"],
            long_valued: &["class", "classdata", "pid", "pgid", "uid"],
        },
        "stdbuf" => Spec {
            flags: "",
            valued: "ioe",
            long_flags: &[],
            long_valued: &["input", "output", "error"],
        },
        "strace" => Spec::short("cCdfFhiqrtTvVwxyzkZn", "abeEIoOpPsSuUX"),
        "ltrace" => Spec::short("bcCfhiLrStTV", "aADeFlnopsuwx"),
        "timeout" => {
            const TIMEOUT: Spec = Spec {
                flags: "v",
                valued: "sk",
                long_flags: &["foreground", "preserve-status", "verbose"],
                long_valued: &["signal", "kill-after"],
            };
            return Some(after_options(args, &TIMEOUT).map(skip_one));
        }
        "flock" => {
            // `-c`/`--command` runs a command string: not modelled, so it asks.
            const FLOCK: Spec = Spec {
                flags: "sxunoeF",
                valued: "wE",
                long_flags: &[
                    "shared",
                    "exclusive",
                    "unlock",
                    "nonblock",
                    "close",
                    "no-fork",
                    "verbose",
                ],
                long_valued: &["timeout", "wait", "conflict-exit-code"],
            };
            return Some(after_options(args, &FLOCK).map(skip_one));
        }
        "chroot" => {
            const CHROOT: Spec = Spec {
                flags: "",
                valued: "",
                long_flags: &["skip-chdir"],
                long_valued: &["userspec", "groups"],
            };
            return Some(after_options(args, &CHROOT).map(skip_one));
        }
        "wsl" => {
            // `-e`/`--exec` runs the words after it without a shell.
            let exec = args
                .iter()
                .position(|word| matches!(word.text.as_str(), "-e" | "--exec"));
            let options = exec.map_or(args, |at| &args[..at]);
            const WSL: Spec = Spec {
                flags: "",
                valued: "du",
                long_flags: &["system"],
                long_valued: &["distribution", "user", "cd", "shell-type"],
            };
            let rest = after_options(options, &WSL);
            return Some(match exec {
                Some(at) => rest.filter(|rest| rest.is_empty()).map(|_| &args[at + 1..]),
                None => rest,
            });
        }
        _ => return None,
    };
    Some(after_options(args, &spec))
}

pub(super) fn danger(name: &str, args: &[Word], piped: bool) -> Option<Danger> {
    if delete(name, args, piped) {
        Some(Danger::Delete)
    } else if name == "git" {
        super::git::danger(args)
    } else if disk(name, args) {
        Some(Danger::Disk)
    } else if permissions(name, args) {
        Some(Danger::Permissions)
    } else if matches!(name, "start-process" | "saps" | "start")
        && args
            .iter()
            .any(|word| word.text.eq_ignore_ascii_case("runas"))
    {
        Some(Danger::Privilege)
    } else {
        None
    }
}

const DELETES: [&str; 7] = ["rm", "remove-item", "ri", "del", "erase", "rd", "rmdir"];

fn delete(name: &str, args: &[Word], piped: bool) -> bool {
    match name {
        "rimraf" | "shred" | "srm" | "wipe" | "clear-recyclebin" => true,
        // `ls | Remove-Item` and `xargs rm` delete everything they are fed.
        // A wildcard deletes whatever it matches: `rm *`, `del *.*`.
        _ if DELETES.contains(&name) => {
            piped
                || args.iter().any(|word| word.text.contains(['*', '?']))
                || args
                    .iter()
                    .take_while(|word| word.text != "--")
                    .any(|word| delete_flag(&word.text))
        }
        "rsync" => args
            .iter()
            .any(|word| word.text.starts_with("--delete") || word.text == "--remove-source-files"),
        _ => false,
    }
}

/// `rm -r/-f` clusters, PowerShell `-Recurse`/`-Force` and their prefixes,
/// and cmd's `/s`, `/f` and `/q`.
fn delete_flag(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    if let Some(long) = lower.strip_prefix("--") {
        return matches!(long, "recursive" | "force" | "recurse");
    }
    if let Some(flags) = lower.strip_prefix('-') {
        let flags = flags.split(':').next().unwrap_or_default();
        if !flags.is_empty() && flags.len() <= 5 && flags.chars().all(|c| "dfirv".contains(c)) {
            return flags.contains(['r', 'f']);
        }
        return !flags.is_empty()
            && ("recurse".starts_with(flags) || (flags.len() >= 2 && "force".starts_with(flags)));
    }
    matches!(lower.as_str(), "/s" | "/f" | "/q")
}

fn disk(name: &str, args: &[Word]) -> bool {
    let any = |test: &dyn Fn(&str) -> bool| {
        args.iter()
            .any(|word| test(&word.text.to_ascii_lowercase()))
    };
    name.starts_with("mkfs")
        || matches!(
            name,
            "mke2fs"
                | "mkswap"
                | "mkdosfs"
                | "mkntfs"
                | "fdisk"
                | "sfdisk"
                | "cfdisk"
                | "gdisk"
                | "sgdisk"
                | "parted"
                | "wipefs"
                | "blkdiscard"
                | "diskpart"
                | "format"
                | "format-volume"
                | "clear-disk"
                | "initialize-disk"
                | "remove-partition"
                | "bcdedit"
                | "shutdown"
                | "reboot"
                | "halt"
                | "poweroff"
                | "stop-computer"
                | "restart-computer"
        )
        || (name == "dd" && any(&|arg| arg.starts_with("of=")))
        || (name == "diskutil"
            && any(&|arg| {
                arg.starts_with("erase")
                    || matches!(
                        arg,
                        "partitiondisk" | "zerodisk" | "randomdisk" | "secureerase" | "reformat"
                    )
            }))
        || (name == "systemctl"
            && any(&|arg| matches!(arg, "poweroff" | "reboot" | "halt" | "kexec")))
        || (matches!(name, "vssadmin" | "wbadmin") && any(&|arg| arg == "delete"))
        || (name == "cipher" && any(&|arg| arg.starts_with("/w")))
        || (name == "hdparm"
            && any(&|arg| arg.starts_with("--security-erase") || arg.starts_with("--write-sector")))
}

fn permissions(name: &str, args: &[Word]) -> bool {
    match name {
        // `chmod -r` removes read permission; only `-R` recurses.
        "chmod" | "chown" | "chgrp" => args.iter().any(|word| {
            word.text == "--recursive"
                || word
                    .text
                    .strip_prefix('-')
                    .is_some_and(|flags| !flags.starts_with('-') && flags.contains('R'))
        }),
        "icacls" => args.iter().any(|word| word.text.eq_ignore_ascii_case("/t")),
        "takeown" => args.iter().any(|word| word.text.eq_ignore_ascii_case("/r")),
        _ => false,
    }
}

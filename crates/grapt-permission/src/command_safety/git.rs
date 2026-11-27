//! Resolve Git's global options before classifying its literal subcommand.
use super::{lexer::Word, Danger};

/// Only modeled global options and builtins have a known command boundary.
/// Config overrides can install aliases; unknown commands can be aliases or
/// external executables, so neither can support an unasked execution or grant.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn arguments(args: &[Word]) -> Result<&[Word], Danger> {
    let mut index = 0;
    while let Some(word) = args.get(index) {
        let text = word.text.as_str();
        if !text.starts_with('-') {
            break;
        }
        match text {
            "-c" | "--config-env" => return Err(Danger::Git),
            _ if text.starts_with("-c") || text.starts_with("--config-env=") => {
                return Err(Danger::Git);
            }
            "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix" => {
                args.get(index + 1).ok_or(Danger::Opaque)?;
                index += 2;
            }
            "-p"
            | "-P"
            | "--paginate"
            | "--no-pager"
            | "--bare"
            | "--no-replace-objects"
            | "--no-optional-locks"
            | "--literal-pathspecs"
            | "--glob-pathspecs"
            | "--noglob-pathspecs"
            | "--icase-pathspecs"
            | "--no-lazy-fetch"
            | "--no-advice"
            | "--version"
            | "--help"
            | "-v"
            | "-h" => index += 1,
            _ if text.starts_with("-C")
                || [
                    "--git-dir=",
                    "--work-tree=",
                    "--namespace=",
                    "--super-prefix=",
                ]
                .iter()
                .any(|prefix| text.starts_with(prefix)) =>
            {
                index += 1
            }
            _ => return Err(Danger::Opaque),
        }
    }
    let rest = &args[index..];
    if rest.first().is_some_and(|word| {
        !matches!(
            word.text.as_str(),
            "add"
                | "am"
                | "apply"
                | "archive"
                | "bisect"
                | "blame"
                | "branch"
                | "bundle"
                | "cat-file"
                | "check-ignore"
                | "check-ref-format"
                | "checkout"
                | "cherry"
                | "cherry-pick"
                | "clean"
                | "clone"
                | "commit"
                | "config"
                | "describe"
                | "diff"
                | "diff-files"
                | "diff-index"
                | "diff-tree"
                | "fetch"
                | "for-each-ref"
                | "format-patch"
                | "fsck"
                | "gc"
                | "grep"
                | "help"
                | "init"
                | "log"
                | "ls-files"
                | "ls-remote"
                | "ls-tree"
                | "merge"
                | "merge-base"
                | "mv"
                | "notes"
                | "pull"
                | "push"
                | "range-diff"
                | "rebase"
                | "reflog"
                | "remote"
                | "repack"
                | "reset"
                | "restore"
                | "rev-list"
                | "rev-parse"
                | "revert"
                | "rm"
                | "shortlog"
                | "show"
                | "show-ref"
                | "sparse-checkout"
                | "stash"
                | "status"
                | "submodule"
                | "switch"
                | "symbolic-ref"
                | "tag"
                | "update-index"
                | "update-ref"
                | "verify-commit"
                | "verify-tag"
                | "version"
                | "worktree"
        )
    }) {
        return Err(Danger::Opaque);
    }
    // These builtins themselves wrap shell commands rather than just Git
    // arguments. Their executed text cannot form a literal argv grant.
    if rest
        .first()
        .is_some_and(|command| match command.text.as_str() {
            "submodule" => rest.iter().any(|word| word.text == "foreach"),
            "bisect" | "rebase" => true,
            _ => false,
        })
    {
        return Err(Danger::Opaque);
    }
    Ok(rest)
}

pub(super) fn danger(args: &[Word]) -> Option<Danger> {
    let args = match arguments(args) {
        Ok(args) => args,
        Err(danger) => return Some(danger),
    };
    let subcommand = args.first()?;
    let rest = &args[1..];
    let has = |options: &[&str]| {
        rest.iter()
            .any(|word| options.contains(&word.text.as_str()))
    };
    let short = |letter: char| {
        rest.iter().any(|word| {
            word.text
                .strip_prefix('-')
                .is_some_and(|flags| !flags.starts_with('-') && flags.contains(letter))
        })
    };
    let positionals = || rest.iter().filter(|word| !word.text.starts_with('-'));
    let destructive = match subcommand.text.as_str() {
        "reset" => has(&["--hard"]),
        "clean" => !has(&["--dry-run"]) && !short('n'),
        "push" => {
            has(&[
                "--force",
                "--delete",
                "--mirror",
                "--prune",
                "--force-if-includes",
            ]) || rest
                .iter()
                .any(|word| word.text.starts_with("--force-with-lease"))
                || short('f')
                || short('d')
                || positionals().any(|word| {
                    word.text.starts_with('+')
                        || (word.text.starts_with(':') && word.text.len() > 1)
                })
        }
        "branch" => {
            short('D')
                || short('M')
                || short('C')
                || ((has(&["--delete"]) || short('d')) && (has(&["--force"]) || short('f')))
        }
        // `git checkout NAME` restores the file NAME when one exists, and
        // nothing here can tell a file from a branch: only branch creation
        // with at most a start point stays unasked (`git switch` never discards).
        "checkout" => {
            let creates = has(&["-b", "-B", "--orphan"]);
            let paths = positionals().count() - usize::from(creates && positionals().count() > 0);
            let allowed = if creates { 1 } else { 0 };
            has(&["--", "--force", "--ours", "--theirs", "--patch"])
                || short('f')
                || short('p')
                || positionals().any(|word| {
                    word.text == "."
                        || word.text.starts_with("./")
                        || word.text.starts_with(":/")
                        || word.text.contains('*')
                })
                || paths > allowed
        }
        "restore" => {
            let staged = has(&["--staged"]) || short('S');
            let worktree = has(&["--worktree"]) || short('W');
            !staged || worktree
        }
        "switch" => {
            has(&["--discard-changes", "--force", "--force-create"]) || short('f') || short('C')
        }
        "stash" => rest
            .first()
            .is_some_and(|word| matches!(word.text.as_str(), "drop" | "clear")),
        "reflog" => rest
            .first()
            .is_some_and(|word| matches!(word.text.as_str(), "expire" | "delete")),
        "rm" => has(&["--force"]) || short('f'),
        // Only literal builtins reach here; aliases and external commands
        // were rejected by `arguments`.
        _ => false,
    };
    destructive.then_some(Danger::Git)
}

//! Worktree commands are operator effects, never model prompts.
use super::*;
use grapt_api::u3a::{BoundedString, WorkspaceWorktreeRequestDto};

const USAGE: &str = "Use /worktree create <destination> [--copy <JSON array of relative files>], merge <checkout>, remove <checkout>, or open <absolute backend checkout path>. Paths may contain spaces or be JSON quoted. Setup copies: explicit untracked/ignored files from repo root only, with guarded Unix/Windows access. Merge: committed fast-forward only into selected source. Remove: no dirty/untracked/ignored/unmerged/current/unrelated trees; no force. Open creates a NEW session; use the absolute path printed by create.";

enum Action {
    Manage(WorkspaceWorktreeRequestDto),
    Open(String),
}

fn path(text: &str) -> Result<String, &'static str> {
    let text = text.trim();
    let value = if text.starts_with('"') {
        serde_json::from_str::<String>(text).map_err(|_| "Invalid JSON-quoted worktree path.")?
    } else {
        text.to_owned()
    };
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(
            "Worktree paths must be nonempty, within 4096 bytes, with no control characters.",
        );
    }
    Ok(value)
}

fn bounded(value: String) -> Result<BoundedString<4096>, &'static str> {
    BoundedString::try_new(value).map_err(|_| "Worktree path exceeds its bound.")
}

fn parse(args: &str) -> Result<Action, &'static str> {
    // Bound before allocating a JSON list; the backend imposes copy/file-size
    // and containment rules. There is deliberately no shell-style parser here.
    if args.len() > 140_000 {
        return Err("Worktree command exceeds its input bound.");
    }
    let (action, rest) = args.trim().split_once(char::is_whitespace).ok_or(USAGE)?;
    Ok(match action {
        "create" => {
            let (destination, copies) = rest.split_once(" --copy ").unwrap_or((rest, "[]"));
            let setup: Vec<String> = serde_json::from_str(copies)
                .map_err(|_| "--copy requires a JSON array of relative file paths.")?;
            if setup.len() > 32 {
                return Err("At most 32 explicit setup files may be copied.");
            }
            let setup_files = setup
                .into_iter()
                .map(|file| {
                    if file.is_empty() || file.chars().any(char::is_control) {
                        return Err(
                            "Setup file paths must be nonempty and contain no control characters.",
                        );
                    }
                    bounded(file)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Action::Manage(WorkspaceWorktreeRequestDto::Create {
                destination: bounded(path(destination)?)?,
                setup_files,
            })
        }
        "merge" => Action::Manage(WorkspaceWorktreeRequestDto::Merge {
            directory: bounded(path(rest)?)?,
        }),
        "remove" => Action::Manage(WorkspaceWorktreeRequestDto::Remove {
            directory: bounded(path(rest)?)?,
        }),
        "open" => Action::Open(path(rest)?),
        _ => return Err(USAGE),
    })
}

pub(super) fn execute<C: Connection>(client: &mut Client<C>, args: &str, done: &mut Completion) {
    let result = match parse(args) {
        Ok(Action::Manage(request)) => client.begin_worktree(request),
        Ok(Action::Open(directory)) => client.create_session_in(&directory),
        Err(error) => {
            done.notice = error.into();
            return;
        }
    };
    match result {
        Ok(Some(lines)) => {
            done.consume = true;
            done.view = Some(View::Text(lines));
        }
        Ok(None) => done.notice = "Worktree operation in progress; draft kept. Do not retry; cancellation is not rollback.".into(),
        Err(error) => {
            done.notice = error.to_string();
            done.worktree_failure(client.worktree_request_details());
        }
    }
}

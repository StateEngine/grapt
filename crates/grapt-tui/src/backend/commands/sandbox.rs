//! Compatibility repair is explicit native consent, never a model or shell task.
use super::*;
use grapt_client::wire::Spelled;

const USAGE: &str = "Use /sandbox worktree-config to preview, then /sandbox worktree-config create confirm to consent to creating that exact empty file. Changing sessions requires a fresh preview.";

pub(super) fn execute<C: Connection>(client: &mut Client<C>, args: &str, done: &mut Completion) {
    done.session = client.selected().cloned();
    let words: Vec<_> = args.split_whitespace().collect();
    let create = match words.as_slice() {
        ["worktree-config"] => false,
        ["worktree-config", "create", "confirm"] => true,
        _ => {
            done.notice = USAGE.into();
            return;
        }
    };
    let result = if create {
        client.confirm_sandbox_worktree_config_create()
    } else {
        client.preview_sandbox_worktree_config()
    };
    match result {
        Ok(reply) => {
            let mut rows = vec![if create {
                "SANDBOX WORKTREE CONFIG — creation result".into()
            } else {
                "SANDBOX WORKTREE CONFIG — read-only preview".into()
            }];
            if let Some(session) = &done.session {
                rows.push(format!("Session: {}", session.spelled()));
            }
            if let Some(path) = &reply.path {
                rows.push(format!(
                    "Exact backend path: {}",
                    serde_json::Value::String(path.clone())
                ));
            }
            rows.extend(reply.text.lines().map(str::to_owned));
            if !create && reply.missing {
                rows.push("No file written. Opening or closing this pane is NOT consent.".into());
                rows.push("To consent to creating only this empty file, close the pane and enter /sandbox worktree-config create confirm. Sandbox permissions remain unchanged.".into());
            } else if reply.missing {
                rows.push("Still missing; creation was not confirmed. Inspect the result, then preview again before any new attempt.".into());
            } else if reply.text.is_empty() {
                rows.push("No missing worktree-config file reported; nothing to create.".into());
            }
            done.consume = true;
            done.notice = if create {
                "Creation response received; see the result. No automatic retry."
            } else {
                "Sandbox preview opened; no file written."
            }
            .into();
            done.view = Some(View::Text(rows));
        }
        Err(error) => {
            done.notice = error.to_string();
            if create {
                let rows = vec![
                    "SANDBOX WORKTREE CONFIG — request not confirmed".into(),
                    done.notice.clone(),
                    "No automatic retry. A sent request may have created the file; inspect it and run /sandbox worktree-config again before any new attempt.".into(),
                ];
                done.view = Some(View::NoticeText {
                    notice: "Sandbox file creation not confirmed; inspect the result. Draft kept."
                        .into(),
                    rows,
                });
            }
        }
    }
}

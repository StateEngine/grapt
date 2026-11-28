//! Apache-2.0, Copyright 2025 OpenAI.
//! Modified by Grapt: existing server preset selection and effective-policy readback.
use super::{Client, Completion, Connection, View};
use grapt_client::preset::Preset;

const USAGE: &str = "Use /permissions [inspect|author|maintainer]; draft kept.";

fn refuse(done: &mut Completion, message: impl Into<String>) {
    done.notice = message.into();
    done.view = Some(View::Notice(done.notice.clone()));
}

pub(super) fn execute<C: Connection>(client: &mut Client<C>, args: &str, done: &mut Completion) {
    let mut args = args.split_whitespace();
    let name = args.next();
    let confirmation = args.next();
    if args.next().is_some() {
        return refuse(done, USAGE);
    }
    let selected = match (name, confirmation) {
        (None, None) => None,
        (Some("inspect"), None) => Some(Preset::Inspect),
        (Some("author"), None) => Some(Preset::Author),
        (Some("maintainer"), None) => Some(Preset::Maintainer),
        (Some("bypass"), Some("confirm")) => Some(Preset::Bypass),
        (Some("bypass"), None) => return refuse(done,
            "Bypass disables permission checks, including configuration protections. To select it explicitly, run /permissions bypass confirm."),
        _ => return refuse(done, USAGE),
    };
    // Settings need an ordinary session, but invalid/unconfirmed choices above
    // must not create one or issue any permission write.
    if let Err(error) = client.create_draft_session() {
        return refuse(done, error);
    }
    let result = match selected {
        Some(preset) => client.select_permission_preset(preset),
        None => client.refresh_permission_preset(),
    };
    if let Err(error) = result {
        return refuse(done, format!(
            "Permission settings unavailable: {error}. Use /permissions to read the effective policy before continuing."
        ));
    }
    done.consume = true;
    if selected.is_none() {
        done.view =
            crate::grants::Snapshot::read(client, "Select an exact grant to withdraw it.".into())
                .map(View::Grants);
        return;
    }
    done.view = Some(View::Text(vec![
        "PERMISSIONS".into(),
        client
            .preset_line()
            .unwrap_or_else(|| "Preset unavailable".into()),
        "The server's effective policy governs subsequent work.".into(),
        "/permissions inspect — read and search; deny mutations".into(),
        "/permissions author — file changes; no command execution".into(),
        "/permissions maintainer — read and workspace edits; ask for shell, overwrites and deletes"
            .into(),
        "/permissions bypass confirm — disable permission checks explicitly".into(),
    ]));
}

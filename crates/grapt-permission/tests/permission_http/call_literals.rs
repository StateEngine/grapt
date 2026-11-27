use super::*;
use grapt_permission::{
    preset::{Compose, Effect, Layer, LayerName, Rule},
    PermissionCreateEffect,
};

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn concrete_approvals_do_not_expand_call_metacharacters() -> TestResult {
    let cases = [
        (
            "shell",
            r#"python -c "print('*')""#,
            r#"python -c "print('probe');open('approval-probe-marker.txt','x').write('probe');print('probe')""#,
        ),
        (
            "shell",
            r#"python -c "print('?')""#,
            r#"python -c "print('p')""#,
        ),
        (
            "shell",
            r#"python -c "print('[*]')""#,
            r#"python -c "print('[probe]')""#,
        ),
        (
            "shell",
            r#"python -c "print('[a]')""#,
            r#"python -c "print('a')""#,
        ),
        (
            "shell",
            r#"python -c "print('C:\*')""#,
            r#"python -c "print('C:/probe')""#,
        ),
        ("glob", "*.rs", "secret.txt"),
        ("read", "literal[?].txt", "literal[a].txt"),
        (
            grapt_permission::SANDBOX_WRITE_ACTION,
            "/outside/approved[*].txt",
            "/outside/approved[probe].txt",
        ),
        (
            grapt_permission::SANDBOX_WRITE_ACTION,
            r"C:\outside\approved[?].txt",
            "C:/outside/approved[a].txt",
        ),
        (
            "mcp__fixture__tool",
            "fixture/tool[*]",
            "fixture/tool[probe]",
        ),
    ];
    for reply in [PermissionHttpReply::Session, PermissionHttpReply::Always] {
        for (index, (action, approved, probe)) in cases.iter().enumerate() {
            let session: SessionId = "ses_literal_approval".parse()?;
            let client = AuthenticatedClientId::new("literal-approval-client").ok_or("client")?;
            let owner = PermissionOwner {
                subject: "human",
                client: &client,
            };
            let persisted = (reply == PermissionHttpReply::Always).then(|| {
                std::env::temp_dir().join(format!(
                    "grapt-literal-always-{}-{index}",
                    std::process::id()
                ))
            });
            if let Some(root) = &persisted {
                std::fs::create_dir(root)?;
            }
            let database = persisted.as_ref().map(|root| root.join("existing.db"));
            let mut store = store_at(database.as_deref(), &session)?;
            let mut state = PermissionHttpState::default();
            let layers = [Layer {
                name: LayerName::Config,
                rules: vec![Rule::new(*action, "*", Effect::Ask)],
                floor: false,
                compose: Compose::Narrow,
            }];
            let id: PermissionId = "per_literal_call".parse()?;
            let resources = [(*approved).to_owned()];
            assert_eq!(
                state.create(
                    &mut store,
                    &owner,
                    &event("evt_literal_ask")?,
                    &id,
                    &session,
                    &agent()?,
                    &layers,
                    &project()?,
                    action,
                    &resources,
                    Some(&resources),
                    None,
                    None,
                    1
                )?,
                PermissionCreateEffect::Ask
            );
            state.reply(
                &mut store,
                PermissionReplyCommand {
                    owner: owner.clone(),
                    session: &session,
                    id: &id,
                    reply,
                    project: Some(&project()?),
                    created_ms: 2,
                    decision: decision("literal_call")?,
                },
                || Err(PermissionHttpError::Identity("unexpected cascade".into())),
            )?;
            if reply == PermissionHttpReply::Always {
                // The durable format is unchanged. Reopen a pre-existing row,
                // including literal '*' text, through the new interpretation.
                let saved = PermissionHttpState::saved_rules(&store, Some(&project()?))?;
                drop(store);
                store = store_at(database.as_deref(), &session)?;
                state = PermissionHttpState::default();
                assert_eq!(
                    PermissionHttpState::saved_rules(&store, Some(&project()?))?,
                    saved,
                    "opening existing approvals rewrote their stored text"
                );
            }
            assert_eq!(
                state.preview_tool(
                    &store,
                    &session,
                    &layers,
                    &project()?,
                    action,
                    &resources,
                    None
                )?,
                PermissionCreateEffect::Allow,
                "the approved bytes stopped matching: {reply:?}, case {index}"
            );
            assert_eq!(
                state.preview_tool(
                    &store,
                    &session,
                    &layers,
                    &project()?,
                    action,
                    &[(*probe).to_owned()],
                    None
                )?,
                PermissionCreateEffect::Ask,
                "concrete approval expanded call text: {reply:?}, case {index}"
            );
            drop(store);
            if let Some(root) = persisted {
                std::fs::remove_dir_all(root)?;
            }
        }
    }
    Ok(())
}

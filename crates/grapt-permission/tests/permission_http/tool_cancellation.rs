#[path = "../../../../test-support/commit_fault.rs"]
mod commit_fault;
use super::*;

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn seat01_no_approver_refuses_only_ask_before_durable_admission() -> TestResult {
    use grapt_permission::preset::{Compose, Effect, Layer, LayerName, Rule};
    let session: SessionId = "ses_headless_approval".parse()?;
    let client = AuthenticatedClientId::new("headless-client").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "headless-owner",
        client: &client,
    };
    let mut store = store_at(None, &session)?;
    let mut state = PermissionHttpState::default();
    let authority = ToolPermissionAuthority::default();
    for ordinal in 0..=MAX_PENDING_TOOL_PERMISSIONS {
        let result = state.begin_tool(
            &mut store,
            &authority,
            tool_capability(),
            &owner,
            &event(&format!("evt_headless_{ordinal}"))?,
            &format!("per_headless_{ordinal}").parse()?,
            &session,
            &agent()?,
            &[],
            &project()?,
            "read",
            &["file".into()],
            None,
            None,
            None,
            0,
            None,
            false,
        );
        assert!(
            matches!(result, Err(PermissionHttpError::ApprovalUnavailable)),
            "headless Ask was not immediately refused: {result:?}"
        );
    }
    let asks: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'permission.asked.2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(asks, 0, "headless Ask left durable orphans");
    for effect in [Effect::Allow, Effect::Deny] {
        let layers = [Layer {
            name: LayerName::Config,
            compose: Compose::Override,
            floor: false,
            rules: vec![Rule::new("read", "*", effect)],
        }];
        let result = state.begin_tool(
            &mut store,
            &authority,
            tool_capability(),
            &owner,
            &event("evt_headless_decided")?,
            &"per_headless_decided".parse()?,
            &session,
            &agent()?,
            &layers,
            &project()?,
            "read",
            &["file".into()],
            None,
            None,
            None,
            0,
            None,
            false,
        );
        if effect == Effect::Deny {
            assert!(
                matches!(result, Err(PermissionHttpError::Denied { .. })),
                "headless check replaced a denial"
            );
        } else {
            let pending = result?;
            assert!(!pending.awaits_decision());
            assert_eq!(pending.consume_and_run(|_| "ran once").await?, "ran once");
        }
    }
    // Refusals consumed no process-local capacity: attachment can admit now.
    let pending = state.begin_tool(
        &mut store,
        &authority,
        tool_capability(),
        &owner,
        &event("evt_attached")?,
        &"per_attached".parse()?,
        &session,
        &agent()?,
        &[],
        &project()?,
        "read",
        &["file".into()],
        None,
        None,
        None,
        1,
        None,
        true,
    )?;
    assert!(pending.awaits_decision());
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn seat01_disconnect_cancellation_is_durable_and_preserves_prior_decisions() -> TestResult {
    let session: SessionId = "ses_approval_disconnect".parse()?;
    let client = AuthenticatedClientId::new("approval-client").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "approval-owner",
        client: &client,
    };
    let mut store = store_at(None, &session)?;
    let mut state = PermissionHttpState::default();
    let authority = ToolPermissionAuthority::default();
    for (suffix, replied_first) in [("cancelled", false), ("replied", true)] {
        let id: PermissionId = format!("per_disconnect_{suffix}").parse()?;
        let pending = state.begin_tool(
            &mut store,
            &authority,
            tool_capability(),
            &owner,
            &event(&format!("evt_disconnect_ask_{suffix}"))?,
            &id,
            &session,
            &agent()?,
            &[],
            &project()?,
            "read",
            &["file".into()],
            None,
            None,
            None,
            0,
            None,
            true,
        )?;
        if replied_first {
            let mut reply = state.reply(
                &mut store,
                PermissionReplyCommand {
                    owner: owner.clone(),
                    session: &session,
                    id: &id,
                    reply: PermissionHttpReply::Once,
                    project: None,
                    created_ms: 1,
                    decision: decision(suffix)?,
                },
                || Err(PermissionHttpError::Identity("no cascade".into())),
            )?;
            authority.settle(reply.take_committed().ok_or("reply proof")?)?;
        }
        let cancelled = state.cancel_tool_because(
            &mut store,
            &owner,
            &session,
            &id,
            &event(&format!("evt_disconnect_cancel_{suffix}"))?,
            2,
            grapt_permission::CancelReason::ApprovalUnavailable,
        )?;
        assert_eq!(cancelled.is_some(), !replied_first);
        if let Some(committed) = cancelled {
            authority.cancel(committed);
        }
        let ran = std::sync::atomic::AtomicBool::new(false);
        let result = pending
            .consume_and_run(|_| ran.store(true, std::sync::atomic::Ordering::SeqCst))
            .await;
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), replied_first);
        if !replied_first {
            assert!(matches!(
                result,
                Err(ToolAuthorizationError::ApprovalUnavailable(_))
            ));
            let data: String = store.connection().query_row(
                "SELECT data FROM event WHERE id = ?1",
                [format!("evt_disconnect_cancel_{suffix}")],
                |row| row.get(0),
            )?;
            let event = grapt_permission::PermissionEvent::decode(
                "permission.cancelled",
                1,
                &serde_json::from_str(&data)?,
            )?;
            assert!(matches!(
                event,
                Some(grapt_permission::PermissionEvent::Cancelled {
                    reason: grapt_permission::CancelReason::ApprovalUnavailable,
                    ..
                })
            ));
            let mut late_reply = state.reply(
                &mut store,
                PermissionReplyCommand {
                    owner: owner.clone(),
                    session: &session,
                    id: &id,
                    reply: PermissionHttpReply::Once,
                    project: None,
                    created_ms: 3,
                    decision: decision("too_late")?,
                },
                || Err(PermissionHttpError::Identity("no cascade".into())),
            )?;
            assert_eq!(late_reply.outcome(), CasOutcome::AlreadyTerminal);
            assert!(
                late_reply.take_committed().is_none(),
                "reconnect resurrected a cancelled request"
            );
        } else {
            result?;
        }
    }
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn tool_cancellation_requires_owner_and_commit_then_releases_capacity_without_effect(
) -> TestResult {
    let session: SessionId = "ses_tool_cancel".parse()?;
    let client = AuthenticatedClientId::new("client-tool-cancel").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-tool-cancel",
        client: &client,
    };
    let mut store = store_at(None, &session)?;
    let mut state = PermissionHttpState::default();
    let authority = ToolPermissionAuthority::default();
    let mut pending = Vec::new();
    for ordinal in 0..MAX_PENDING_TOOL_PERMISSIONS {
        let id: PermissionId = format!("per_cancel_{ordinal}").parse()?;
        let effect = state.begin_tool(
            &mut store,
            &authority,
            tool_capability(),
            &owner,
            &event(&format!("evt_cancel_ask_{ordinal}"))?,
            &id,
            &session,
            &agent()?,
            &[],
            &project()?,
            "read",
            &["cancel.txt".to_owned()],
            None,
            None,
            None,
            0,
            None,
            true,
        )?;
        assert!(effect.awaits_decision());
        pending.push(effect);
    }
    let id: PermissionId = "per_cancel_0".parse()?;
    let cancel_event = event("evt_tool_cancel")?;
    let foreign = PermissionOwner {
        subject: "another-human",
        client: &client,
    };
    assert!(state
        .cancel_tool(&mut store, &foreign, &session, &id, &cancel_event, 1)
        .is_err());
    let status = |store: &EventStore| -> Result<String, rusqlite::Error> {
        store.connection().query_row(
            "SELECT status FROM permission_request WHERE id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )
    };
    assert_eq!(status(&store)?, "pending");

    // Refuse the canonical cancellation commit. No proof can escape
    // a failed transaction and the pending row must remain recoverable.
    let fault = commit_fault::CommitFault::install(
        &mut store,
        rusqlite::hooks::Action::SQLITE_INSERT,
        "event",
        0,
    )?;
    assert!(state
        .cancel_tool(&mut store, &owner, &session, &id, &cancel_event, 2)
        .is_err());
    assert_eq!(status(&store)?, "pending");
    let overflow: PermissionId = "per_cancel_overflow".parse()?;
    assert!(matches!(
        state.begin_tool(
            &mut store,
            &authority,
            tool_capability(),
            &owner,
            &event("evt_cancel_overflow")?,
            &overflow,
            &session,
            &agent()?,
            &[],
            &project()?,
            "read",
            &["cancel.txt".to_owned()],
            None,
            None,
            None,
            3,
            None,
            true,
        ),
        Err(PermissionHttpError::ToolAuthority(
            ToolAuthorizationError::PendingBound(_)
        ))
    ));
    drop(fault);

    let committed = state
        .cancel_tool(&mut store, &owner, &session, &id, &cancel_event, 4)?
        .ok_or("cancellation omitted its committed proof")?;
    assert_eq!(status(&store)?, "cancelled");
    authority.cancel(committed);
    let cancelled = pending
        .remove(0)
        .consume_and_run(|_| panic!("cancelled effect executed"))
        .await;
    assert!(matches!(cancelled, Err(ToolAuthorizationError::Cancelled(found)) if found == id));
    assert!(state
        .cancel_tool(
            &mut store,
            &owner,
            &session,
            &id,
            &event("evt_cancel_repeat")?,
            5
        )?
        .is_none());
    let mut late_reply = state.reply(
        &mut store,
        PermissionReplyCommand {
            owner: owner.clone(),
            session: &session,
            id: &id,
            reply: PermissionHttpReply::Once,
            project: None,
            created_ms: 6,
            decision: decision("cancel_late")?,
        },
        || Err(PermissionHttpError::Identity("no cascade".to_owned())),
    )?;
    assert_eq!(late_reply.outcome(), CasOutcome::AlreadyTerminal);
    assert!(
        late_reply.take_committed().is_none(),
        "late approval resurrected tool authority"
    );

    pending.push(state.begin_tool(
        &mut store,
        &authority,
        tool_capability(),
        &owner,
        &event("evt_cancel_replacement")?,
        &overflow,
        &session,
        &agent()?,
        &[],
        &project()?,
        "read",
        &["cancel.txt".to_owned()],
        None,
        None,
        None,
        7,
        None,
        true,
    )?);
    assert_eq!(pending.len(), MAX_PENDING_TOOL_PERMISSIONS);
    let cancellations: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'permission.cancelled.1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        cancellations, 1,
        "failed or repeated cancellation appended another terminal"
    );
    Ok(())
}

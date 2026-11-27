use std::path::Path;

use grapt_permission::{
    DecisionIds, PermissionHttpError, PermissionHttpReply, PermissionHttpSource,
    PermissionHttpState, PermissionOwner, PermissionReplyCommand, ToolAuthorizationError,
    ToolPermissionAuthority, MAX_PENDING_TOOL_PERMISSIONS,
};
use grapt_store::{AggregateId, AppendInput, EventLog, EventStore};
use grapt_types::{
    AgentId, AuthenticatedClientId, CapabilityId, CasOutcome, DecisionNonce, EffectClass,
    EgressAuthority, EventId, EventVersion, FilesystemAuthority, PermissionId, ProjectId,
    RealizedEnforcement, RequestId, ResolvedCapability, SessionId,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn store_at(
    path: Option<&Path>,
    session: &SessionId,
) -> Result<EventStore, Box<dyn std::error::Error>> {
    let mut store = match path {
        Some(path) => EventStore::open(path, "0.0.1")?,
        None => EventStore::open_in_memory()?,
    };
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    PermissionHttpState::initialize(&mut store)?;
    Ok(store)
}

fn project() -> Result<ProjectId, Box<dyn std::error::Error>> {
    ProjectId::new("prj_http").ok_or_else(|| "project id".into())
}

fn agent() -> Result<AgentId, Box<dyn std::error::Error>> {
    AgentId::new("agt_http".to_owned()).ok_or_else(|| "agent id".into())
}

fn event(raw: &str) -> Result<EventId, Box<dyn std::error::Error>> {
    Ok(raw.parse()?)
}

fn decision(suffix: &str) -> Result<DecisionIds, Box<dyn std::error::Error>> {
    Ok(DecisionIds {
        event: format!("evt_reply{suffix}").parse()?,
        request: RequestId::new(format!("req-{suffix}")).ok_or("request id")?,
        nonce: DecisionNonce::new(format!("nonce-{suffix}")).ok_or("nonce")?,
    })
}

fn tool_capability() -> CapabilityId {
    CapabilityId::derive(&ResolvedCapability::new(
        EffectClass::FilesystemRead,
        None,
        FilesystemAuthority::default(),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    ))
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn sixty_fifth_tool_ask_creates_zero_orphan_and_reconciles_truthfully() -> TestResult {
    let session: SessionId = "ses_tool_capacity".parse()?;
    let client = AuthenticatedClientId::new("client-tool-capacity").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-tool-capacity",
        client: &client,
    };
    let mut store = store_at(None, &session)?;
    let mut state = PermissionHttpState::default();
    let authority = ToolPermissionAuthority::default();
    let project = project()?;
    let agent = agent()?;
    let mut pending = Vec::new();
    for ordinal in 0..MAX_PENDING_TOOL_PERMISSIONS {
        let id: PermissionId = format!("per_tool_capacity_{ordinal}").parse()?;
        pending.push(state.begin_tool(
            &mut store,
            &authority,
            tool_capability(),
            &owner,
            &event(&format!("evt_tool_capacity_{ordinal}"))?,
            &id,
            &session,
            &agent,
            &[],
            &project,
            "read",
            &[format!("file-{ordinal}")],
            None,
            None,
            None,
            i64::try_from(ordinal)?,
            None,
            true,
        )?);
    }

    let overflow_id: PermissionId = "per_tool_capacity_64".parse()?;
    let overflow = state.begin_tool(
        &mut store,
        &authority,
        tool_capability(),
        &owner,
        &event("evt_tool_capacity_64")?,
        &overflow_id,
        &session,
        &agent,
        &[],
        &project,
        "read",
        &["file-64".to_owned()],
        None,
        None,
        None,
        64,
        None,
        true,
    );
    assert!(
        matches!(
            overflow,
            Err(PermissionHttpError::ToolAuthority(
                ToolAuthorizationError::PendingBound(MAX_PENDING_TOOL_PERMISSIONS)
            ))
        ),
        "the 65th tool ask did not fail at the process-local capacity boundary"
    );
    let asks: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'permission.asked.2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        asks, 64,
        "the 65th capacity refusal created a durable orphan"
    );
    assert!(
        matches!(
            state.reply(
                &mut store,
                PermissionReplyCommand {
                    owner: owner.clone(),
                    session: &session,
                    id: &overflow_id,
                    reply: PermissionHttpReply::Once,
                    project: None,
                    created_ms: 65,
                    decision: decision("tool_capacity_overflow")?,
                },
                || Err(PermissionHttpError::Identity("no cascade".to_owned())),
            ),
            Err(PermissionHttpError::NotFound(_))
        ),
        "the uncommitted 65th ask falsely accepted a reply"
    );

    let first_id: PermissionId = "per_tool_capacity_0".parse()?;
    let mut reply = state.reply(
        &mut store,
        PermissionReplyCommand {
            owner: owner.clone(),
            session: &session,
            id: &first_id,
            reply: PermissionHttpReply::Once,
            project: None,
            created_ms: 66,
            decision: decision("tool_capacity_first")?,
        },
        || Err(PermissionHttpError::Identity("no cascade".to_owned())),
    )?;
    let committed = reply
        .take_committed()
        .ok_or("the first reply omitted its committed authority proof")?;
    authority.settle(committed)?;
    let first = pending.remove(0);
    assert_eq!(
        first.consume_and_run(|capability| capability).await?,
        tool_capability(),
        "the committed reply did not release exactly its reserved effect"
    );
    let retry = state.reply(
        &mut store,
        PermissionReplyCommand {
            owner: owner.clone(),
            session: &session,
            id: &first_id,
            reply: PermissionHttpReply::Once,
            project: None,
            created_ms: 67,
            decision: decision("tool_capacity_first")?,
        },
        || Err(PermissionHttpError::Identity("no cascade".to_owned())),
    )?;
    assert_eq!(retry.outcome(), CasOutcome::Applied);

    let replacement_id: PermissionId = "per_tool_capacity_replacement".parse()?;
    pending.push(state.begin_tool(
        &mut store,
        &authority,
        tool_capability(),
        &owner,
        &event("evt_tool_capacity_replacement")?,
        &replacement_id,
        &session,
        &agent,
        &[],
        &project,
        "read",
        &["replacement".to_owned()],
        None,
        None,
        None,
        68,
        None,
        true,
    )?);
    assert_eq!(pending.len(), MAX_PENDING_TOOL_PERMISSIONS);
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn failed_ask_commit_rolls_back_its_reserved_capacity() -> TestResult {
    let session: SessionId = "ses_tool_rollback".parse()?;
    let client = AuthenticatedClientId::new("client-tool-rollback").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-tool-rollback",
        client: &client,
    };
    let mut store = store_at(None, &session)?;
    let mut state = PermissionHttpState::default();
    let authority = ToolPermissionAuthority::default();
    let project = project()?;
    let agent = agent()?;
    let mut pending = Vec::new();
    for ordinal in 0..(MAX_PENDING_TOOL_PERMISSIONS - 1) {
        let id: PermissionId = format!("per_tool_rollback_{ordinal}").parse()?;
        pending.push(state.begin_tool(
            &mut store,
            &authority,
            tool_capability(),
            &owner,
            &event(&format!("evt_tool_rollback_{ordinal}"))?,
            &id,
            &session,
            &agent,
            &[],
            &project,
            "read",
            &[format!("rollback-{ordinal}")],
            None,
            None,
            None,
            i64::try_from(ordinal)?,
            None,
            true,
        )?);
    }

    let conflict = event("evt_tool_rollback_conflict")?;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let version = EventVersion::new(1).ok_or("event version")?;
    store
        .append(
            AppendInput {
                id: &conflict,
                aggregate,
                writer,
                kind_name: "session.created",
                version,
                created_ms: 63,
                data: &serde_json::json!({"sessionID": session.as_str()}),
            },
            &mut [],
        )?
        .1
        .discard();
    let conflicted_id: PermissionId = "per_tool_rollback_conflict".parse()?;
    assert!(
        state
            .begin_tool(
                &mut store,
                &authority,
                tool_capability(),
                &owner,
                &conflict,
                &conflicted_id,
                &session,
                &agent,
                &[],
                &project,
                "read",
                &["conflict".to_owned()],
                None,
                None,
                None,
                64,
                None,
                true,
            )
            .is_err(),
        "the conflicting durable ask unexpectedly committed"
    );

    let final_id: PermissionId = "per_tool_rollback_final".parse()?;
    pending.push(state.begin_tool(
        &mut store,
        &authority,
        tool_capability(),
        &owner,
        &event("evt_tool_rollback_final")?,
        &final_id,
        &session,
        &agent,
        &[],
        &project,
        "read",
        &["final".to_owned()],
        None,
        None,
        None,
        65,
        None,
        true,
    )?);
    assert_eq!(
        pending.len(),
        MAX_PENDING_TOOL_PERMISSIONS,
        "the failed durable ask leaked its reserved capacity"
    );
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn ask_list_and_reply_commit_typed_v2_attribution() -> TestResult {
    let session: SessionId = "ses_http".parse()?;
    let id: PermissionId = "per_http".parse()?;
    let client = AuthenticatedClientId::new("client-http").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-alpha",
        client: &client,
    };
    let mut store = store_at(None, &session)?;
    let mut state = PermissionHttpState::default();
    state.create(
        &mut store,
        &owner,
        &event("evt_askhttp")?,
        &id,
        &session,
        &agent()?,
        &[],
        &project()?,
        "write",
        &["/work/file".to_owned()],
        None,
        None,
        None,
        10,
    )?;

    let listed = state.list(&store, &owner, Some(&session))?;
    assert_eq!(listed.requests.len(), 1);
    assert_eq!(listed.requests[0].id, id);
    assert_eq!(listed.requests[0].resources, vec!["/work/file"]);

    let ids = decision("http")?;
    assert_eq!(
        state.reply(
            &mut store,
            PermissionReplyCommand {
                owner: owner.clone(),
                session: &session,
                id: &id,
                reply: PermissionHttpReply::Once,
                project: None,
                created_ms: 11,
                decision: ids.clone(),
            },
            || Err(PermissionHttpError::Identity(
                "unexpected cascade mint".to_owned()
            )),
        )?,
        CasOutcome::Applied
    );
    let (principal, stored_client, request, nonce): (String, String, String, String) =
        store.connection().query_row(
            "SELECT json_extract(data, '$.principal'), json_extract(data, '$.clientID'),
                    json_extract(data, '$.replyRequestID'), json_extract(data, '$.nonce')
             FROM event WHERE type = 'permission.replied.2'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    assert_eq!(
        principal, "human-alpha",
        "v2 reply attribution must preserve the authenticated principal"
    );
    assert_eq!(
        stored_client, "client-http",
        "v2 reply attribution must preserve the authenticated client"
    );
    assert_eq!(
        request, "req-http",
        "v2 reply attribution must preserve the reply request identity"
    );
    assert_eq!(
        nonce, "nonce-http",
        "v2 reply attribution must preserve the reply nonce"
    );
    let retry = DecisionIds {
        event: event("evt_replyretry")?,
        request: ids.request.clone(),
        nonce: ids.nonce.clone(),
    };
    assert_eq!(
        state.reply(
            &mut store,
            PermissionReplyCommand {
                owner: owner.clone(),
                session: &session,
                id: &id,
                reply: PermissionHttpReply::Once,
                project: None,
                created_ms: 12,
                decision: retry,
            },
            || Err(PermissionHttpError::Identity(
                "retry must not mint".to_owned()
            )),
        )?,
        CasOutcome::Applied,
        "an exact client retry must reconcile to the settled outcome"
    );
    let count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'permission.replied.2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1, "an exact retry must not append a second event");
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn wrong_principal_wrong_client_and_second_answer_are_refused() -> TestResult {
    let session: SessionId = "ses_refusal".parse()?;
    let id: PermissionId = "per_refusal".parse()?;
    let suspended: PermissionId = "per_suspended_sibling".parse()?;
    let client = AuthenticatedClientId::new("client-owner").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-owner",
        client: &client,
    };
    let mut store = store_at(None, &session)?;
    let mut state = PermissionHttpState::default();
    state.create(
        &mut store,
        &owner,
        &event("evt_askrefusal")?,
        &id,
        &session,
        &agent()?,
        &[],
        &project()?,
        "read",
        &[],
        None,
        None,
        None,
        20,
    )?;
    state.create(
        &mut store,
        &owner,
        &event("evt_asksuspended")?,
        &suspended,
        &session,
        &agent()?,
        &[],
        &project()?,
        "read",
        &[],
        None,
        None,
        None,
        20,
    )?;
    let tx = store.governed_transaction()?;
    tx.execute(
        "UPDATE permission_request SET status = 'suspended', suspended_reason = 'process-loss'
         WHERE id = ?1",
        [suspended.as_str()],
    )?;
    tx.commit()?.discard();

    let intruder_client = AuthenticatedClientId::new("client-intruder").ok_or("client")?;
    let wrong_principal = PermissionOwner {
        subject: "human-intruder",
        client: &client,
    };
    let wrong_client = PermissionOwner {
        subject: "human-owner",
        client: &intruder_client,
    };
    assert!(
        matches!(
            state.get(&store, &wrong_principal, &session, &id),
            Err(PermissionHttpError::WrongPrincipal(found)) if found == id
        ),
        "wrong principal must be refused"
    );
    assert!(
        matches!(
            state.get(&store, &wrong_client, &session, &id),
            Err(PermissionHttpError::WrongClient(found)) if found == id
        ),
        "wrong client must be refused"
    );

    let mut cascade = Some(decision("cascade")?);
    assert_eq!(
        state.reply(
            &mut store,
            PermissionReplyCommand {
                owner: owner.clone(),
                session: &session,
                id: &id,
                reply: PermissionHttpReply::Reject,
                project: None,
                created_ms: 21,
                decision: decision("first")?,
            },
            || {
                cascade.take().ok_or_else(|| {
                    PermissionHttpError::Identity("unexpected extra decision mint".to_owned())
                })
            },
        )?,
        CasOutcome::Applied
    );
    assert_eq!(
        state.reply(
            &mut store,
            PermissionReplyCommand {
                owner: owner.clone(),
                session: &session,
                id: &id,
                reply: PermissionHttpReply::Once,
                project: None,
                created_ms: 22,
                decision: decision("second")?,
            },
            || {
                Err(PermissionHttpError::Identity(
                    "terminal reply must not mint".to_owned(),
                ))
            },
        )?,
        CasOutcome::AlreadyTerminal,
        "conflicting terminal reply stays already terminal"
    );
    let count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'permission.replied.2'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        count, 2,
        "the primary and suspended sibling settle, while the losing answer appends nothing"
    );
    let sibling_status: String = store.connection().query_row(
        "SELECT status FROM permission_request WHERE id = ?1",
        [suspended.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(
        sibling_status, "replied",
        "reject must cascade over suspended siblings"
    );
    Ok(())
}

#[test]
fn durable_permission_list_converges_after_process_state_is_recreated() -> TestResult {
    let dir = std::env::temp_dir().join(format!(
        "grapt-permission-http-resurrection-{}",
        std::process::id()
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("permission.db");
    let session: SessionId = "ses_resurrection".parse()?;
    let id: PermissionId = "per_resurrection".parse()?;
    let client = AuthenticatedClientId::new("client-resurrection").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-resurrection",
        client: &client,
    };
    {
        let mut store = store_at(Some(&db), &session)?;
        PermissionHttpState::default().create(
            &mut store,
            &owner,
            &event("evt_askresurrection")?,
            &id,
            &session,
            &agent()?,
            &[],
            &project()?,
            "egress",
            &["example.invalid".to_owned()],
            None,
            None,
            None,
            30,
        )?;
    }
    {
        let store = store_at(Some(&db), &session)?;
        let rebuilt = PermissionHttpState::default();
        let rows = rebuilt.list(&store, &owner, Some(&session))?;
        assert_eq!(
            rows.requests.len(),
            1,
            "durable permission truth must survive process-state reconstruction"
        );
        assert_eq!(rows.requests[0].id, id);
        assert!(matches!(
            rows.requests[0].lifecycle,
            grapt_permission::PermissionHttpLifecycle::Pending
        ));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

include!("permission_http/replay_and_saved_rules.rs");
#[path = "permission_http/call_literals.rs"]
mod call_literals;
#[path = "permission_http/rule_revocation.rs"]
mod rule_revocation;
#[path = "permission_http/tool_cancellation.rs"]
mod tool_cancellation;

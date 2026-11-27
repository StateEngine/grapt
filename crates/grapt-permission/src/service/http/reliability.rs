use super::*;
use crate::{PendingToolEffect, ToolAuthorizationError, ToolPermissionAuthority};
use grapt_types::{
    EffectClass, EgressAuthority, FilesystemAuthority, RealizedEnforcement, ResolvedCapability,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn poll_once<F: std::future::Future>(future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => Some(value),
        std::task::Poll::Pending => None,
    }
}

fn decision(index: usize) -> Result<DecisionIds, PermissionHttpError> {
    Ok(DecisionIds {
        event: format!("evt_hr_reply_{index}")
            .parse()
            .map_err(|error: grapt_types::IdError| {
                PermissionHttpError::Identity(error.to_string())
            })?,
        request: RequestId::new(format!("hr-request-{index}"))
            .ok_or_else(|| PermissionHttpError::Identity("request".into()))?,
        nonce: DecisionNonce::new(format!("hr-nonce-{index}"))
            .ok_or_else(|| PermissionHttpError::Identity("nonce".into()))?,
    })
}

struct Fixture {
    store: EventStore,
    state: PermissionHttpState,
    authority: ToolPermissionAuthority,
    session: SessionId,
    client: AuthenticatedClientId,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let session: SessionId = "ses_hr_siblings".parse()?;
        let mut store = EventStore::open_in_memory()?;
        store.elect_writer(&AggregateId::Session(session.clone()))?;
        PermissionHttpState::initialize(&mut store)?;
        Ok(Self {
            store,
            session,
            state: PermissionHttpState::default(),
            authority: ToolPermissionAuthority::default(),
            client: AuthenticatedClientId::new("hr-client").ok_or("client")?,
        })
    }

    fn ask(
        &mut self,
        index: usize,
        subject: &str,
    ) -> Result<(PermissionId, PendingToolEffect), Box<dyn std::error::Error>> {
        let permission: PermissionId = format!("per_hr_{index}").parse()?;
        let capability = CapabilityId::derive(&ResolvedCapability::new(
            EffectClass::FilesystemRead,
            None,
            FilesystemAuthority::default(),
            EgressAuthority::default(),
            RealizedEnforcement::HarnessOnly,
        ));
        let pending = self.state.begin_tool(
            &mut self.store,
            &self.authority,
            capability,
            &PermissionOwner {
                subject,
                client: &self.client,
            },
            &format!("evt_hr_ask_{index}").parse()?,
            &permission,
            &self.session,
            &AgentId::new("agt_hr".to_owned()).ok_or("agent")?,
            &[],
            &ProjectId::new("prj_hr").ok_or("project")?,
            "read",
            &["file".into()],
            None,
            None,
            None,
            0,
            None,
            true,
        )?;
        Ok((permission, pending))
    }

    fn reply(
        &mut self,
        permission: &PermissionId,
        choice: PermissionHttpReply,
        index: usize,
    ) -> Result<PermissionReplyEffect, PermissionHttpError> {
        let mut sibling = index + 100;
        self.state.reply_tools(
            &mut self.store,
            &self.authority,
            PermissionReplyCommand {
                owner: PermissionOwner {
                    subject: "human",
                    client: &self.client,
                },
                session: &self.session,
                id: permission,
                reply: choice,
                project: None,
                created_ms: 1,
                decision: decision(index)?,
            },
            || {
                sibling += 1;
                decision(sibling)
            },
        )
    }
}

#[tokio::test]
async fn hr_p01_cascade_wakes_every_committed_waiter_and_frees_capacity() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut pending = Vec::new();
    for index in 0..crate::MAX_PENDING_TOOL_PERMISSIONS {
        pending.push(fixture.ask(index, "human")?);
    }
    let primary = pending.first().ok_or("missing primary")?.0.clone();
    assert_eq!(
        fixture
            .reply(&primary, PermissionHttpReply::Reject, 0)?
            .outcome(),
        CasOutcome::Applied
    );
    for (id, waiter) in pending {
        let mut effect = 0;
        // An omitted notification fails immediately, rather than hanging the gate.
        let result = poll_once(waiter.consume_and_run(|_| effect += 1))
            .ok_or("committed rejection left an authorization waiter pending")?;
        assert!(
            matches!(result, Err(ToolAuthorizationError::Rejected(rejected)) if rejected == id)
        );
        assert_eq!(effect, 0);
    }
    let terminals: i64 = fixture.store.connection().query_row(
        "SELECT COUNT(*) FROM permission_request WHERE status = 'replied' AND reply = 'reject'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(terminals, 64);
    let (next, waiter) = fixture.ask(64, "human")?;
    assert_eq!(
        fixture
            .reply(&next, PermissionHttpReply::Once, 1)?
            .outcome(),
        CasOutcome::Applied
    );
    assert!(waiter.consume_and_run(|_| ()).await.is_ok());
    Ok(())
}

#[tokio::test]
async fn hr_p02_late_grant_cannot_resurrect_sibling_or_reject_foreign_owner() -> TestResult {
    let mut fixture = Fixture::new()?;
    let (primary, first) = fixture.ask(0, "human")?;
    let (sibling, second) = fixture.ask(1, "human")?;
    let (foreign, other) = fixture.ask(2, "another-human")?;
    fixture.reply(&primary, PermissionHttpReply::Reject, 0)?;
    assert_eq!(
        fixture
            .reply(&sibling, PermissionHttpReply::Once, 1)?
            .outcome(),
        CasOutcome::AlreadyTerminal
    );
    assert!(matches!(
        poll_once(first.consume_and_run(|_| ())),
        Some(Err(ToolAuthorizationError::Rejected(_)))
    ));
    assert!(matches!(
        poll_once(second.consume_and_run(|_| ())),
        Some(Err(ToolAuthorizationError::Rejected(_)))
    ));
    assert_eq!(
        fixture.authority.pending_in(&fixture.session),
        vec![foreign]
    );
    assert!(poll_once(other.consume_and_run(|_| ())).is_none());
    Ok(())
}

#[tokio::test]
async fn hr_p01_failed_cascade_transaction_notifies_nobody() -> TestResult {
    let mut fixture = Fixture::new()?;
    let (primary, first) = fixture.ask(0, "human")?;
    let (_, second) = fixture.ask(1, "human")?;
    let result = fixture.state.reply_tools(
        &mut fixture.store,
        &fixture.authority,
        PermissionReplyCommand {
            owner: PermissionOwner {
                subject: "human",
                client: &fixture.client,
            },
            session: &fixture.session,
            id: &primary,
            reply: PermissionHttpReply::Reject,
            project: None,
            created_ms: 1,
            decision: decision(0)?,
        },
        || {
            Err(PermissionHttpError::Identity(
                "injected cascade failure".into(),
            ))
        },
    );
    assert!(matches!(result, Err(PermissionHttpError::Identity(_))));
    assert_eq!(fixture.authority.pending_in(&fixture.session).len(), 2);
    let terminals: i64 = fixture.store.connection().query_row(
        "SELECT COUNT(*) FROM permission_request WHERE status = 'replied'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        terminals, 0,
        "failed transaction published a primary rejection"
    );
    fixture.reply(&primary, PermissionHttpReply::Reject, 0)?;
    for waiter in [first, second] {
        assert!(matches!(
            poll_once(waiter.consume_and_run(|_| ())),
            Some(Err(ToolAuthorizationError::Rejected(_)))
        ));
    }
    Ok(())
}

#[tokio::test]
async fn hr_p02_grant_committed_before_reject_runs_exactly_once() -> TestResult {
    let mut fixture = Fixture::new()?;
    let (primary, first) = fixture.ask(0, "human")?;
    let (sibling, second) = fixture.ask(1, "human")?;
    fixture.reply(&sibling, PermissionHttpReply::Once, 0)?;
    fixture.reply(&primary, PermissionHttpReply::Reject, 1)?;
    assert!(matches!(
        poll_once(first.consume_and_run(|_| ())),
        Some(Err(ToolAuthorizationError::Rejected(_)))
    ));
    let mut effects = 0;
    second.consume_and_run(|_| effects += 1).await?;
    assert_eq!(effects, 1);
    assert_eq!(
        fixture
            .reply(&sibling, PermissionHttpReply::Once, 2)?
            .outcome(),
        CasOutcome::AlreadyTerminal
    );
    assert!(fixture.authority.pending_in(&fixture.session).is_empty());
    Ok(())
}

#[tokio::test]
async fn hr_p02_cancelled_sibling_stays_cancelled_when_primary_is_rejected() -> TestResult {
    let mut fixture = Fixture::new()?;
    let (primary, first) = fixture.ask(0, "human")?;
    let (sibling, second) = fixture.ask(1, "human")?;
    let committed = fixture
        .state
        .cancel_tool(
            &mut fixture.store,
            &PermissionOwner {
                subject: "human",
                client: &fixture.client,
            },
            &fixture.session,
            &sibling,
            &"evt_hr_cancel".parse()?,
            1,
        )?
        .ok_or("missing committed cancellation")?;
    fixture.authority.cancel(committed);
    fixture.reply(&primary, PermissionHttpReply::Reject, 0)?;
    assert!(matches!(
        poll_once(first.consume_and_run(|_| ())),
        Some(Err(ToolAuthorizationError::Rejected(_)))
    ));
    assert!(matches!(
        poll_once(second.consume_and_run(|_| ())),
        Some(Err(ToolAuthorizationError::Cancelled(_)))
    ));
    assert_eq!(
        fixture
            .reply(&sibling, PermissionHttpReply::Once, 1)?
            .outcome(),
        CasOutcome::AlreadyTerminal
    );
    let status: String = fixture.store.connection().query_row(
        "SELECT status FROM permission_request WHERE id = ?1",
        [sibling.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(status, "cancelled");
    assert!(fixture.authority.pending_in(&fixture.session).is_empty());
    Ok(())
}

#[tokio::test]
async fn hr_a_tool_ask_row_carries_the_tool_name_it_was_asked_with() -> TestResult {
    let mut fixture = Fixture::new()?;
    let (id, _waiter) = fixture.ask(0, "human")?;
    let metadata_json: String = fixture.store.connection().query_row(
        "SELECT h.metadata_json
           FROM permission_request p
           JOIN permission_http_request h ON h.id = p.id
          WHERE p.id = ?1",
        [id.as_str()],
        |row| row.get(0),
    )?;
    let metadata: serde_json::Value = serde_json::from_str(&metadata_json)?;
    assert_eq!(
        metadata.get("tool"),
        Some(&serde_json::json!("read")),
        "the stored ask row must name the tool that asked"
    );
    assert_eq!(metadata["consequences"], serde_json::Value::Null);
    Ok(())
}

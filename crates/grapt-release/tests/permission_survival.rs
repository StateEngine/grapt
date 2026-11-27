use grapt_permission::{
    DecisionIds, PermissionCreateEffect, PermissionHttpError, PermissionHttpReply,
    PermissionHttpSource, PermissionHttpState, PermissionOwner, PermissionReplyCommand,
};
use grapt_release::{fingerprint_file, rehearse};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{
    AgentId, AuthenticatedClientId, ClockAuthority, DecisionNonce, EventId, MigrationId,
    PermissionId, ProjectId, RequestId, SessionId, Timestamp,
};
use std::path::{Path, PathBuf};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct FixedClock;

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_788_000_000_123)
    }
}

fn scratch() -> TestResult<PathBuf> {
    let root = std::env::temp_dir().join(format!("grapt-r6mr-permission-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

fn open_store(path: &Path, session: &SessionId) -> TestResult<EventStore> {
    let mut store = EventStore::open(path, "0.2.0")?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    PermissionHttpState::initialize(&mut store)?;
    Ok(store)
}

fn event(value: &str) -> TestResult<EventId> {
    Ok(value.parse()?)
}

fn decision(suffix: &str) -> TestResult<DecisionIds> {
    Ok(DecisionIds {
        event: format!("evt_r6_reply_{suffix}").parse()?,
        request: RequestId::new(format!("req-r6-{suffix}")).ok_or("request id")?,
        nonce: DecisionNonce::new(format!("nonce-r6-{suffix}")).ok_or("nonce")?,
    })
}

#[test]
fn migration_rebuilds_primary_permission_coordinates_and_always_rule_authority() -> TestResult {
    let root = scratch()?;
    let source = root.join("source.sqlite3");
    let run = root.join("run");
    let session: SessionId = "ses_r6_permission".parse()?;
    let client = AuthenticatedClientId::new("client-r6-permission").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-r6-permission",
        client: &client,
    };
    let agent = AgentId::new("agt_r6_permission".to_owned()).ok_or("agent")?;
    let project = ProjectId::new("prj_r6_permission").ok_or("project")?;
    let rule: PermissionId = "per_r6_rule".parse()?;
    let pending: PermissionId = "per_r6_pending".parse()?;

    {
        let mut store = open_store(&source, &session)?;
        let mut state = PermissionHttpState::default();
        assert_eq!(
            state.create(
                &mut store,
                &owner,
                &event("evt_r6_rule")?,
                &rule,
                &session,
                &agent,
                &[],
                &project,
                "egress",
                &["api.example.test".to_owned()],
                Some(&["*.example.test".to_owned()]),
                None,
                None,
                41,
            )?,
            PermissionCreateEffect::Ask
        );
        state.reply(
            &mut store,
            PermissionReplyCommand {
                owner: owner.clone(),
                session: &session,
                id: &rule,
                reply: PermissionHttpReply::Always,
                project: Some(&project),
                created_ms: 42,
                decision: decision("always")?,
            },
            || Err(PermissionHttpError::Identity("no cascade".to_owned())),
        )?;
        let metadata = serde_json::json!({"reason": "R6 survival witness"});
        let source_coordinate = PermissionHttpSource::Tool {
            message_id: "msg-r6-permission".to_owned(),
            call_id: "call-r6-permission".to_owned(),
        };
        assert_eq!(
            state.create(
                &mut store,
                &owner,
                &event("evt_r6_pending")?,
                &pending,
                &session,
                &agent,
                &[],
                &project,
                "write",
                &["/work/exact".to_owned()],
                Some(&["/work/*".to_owned()]),
                Some(&metadata),
                Some(&source_coordinate),
                43,
            )?,
            PermissionCreateEffect::Ask
        );
    }
    rusqlite::Connection::open(&source)?.execute_batch(
        "PRAGMA foreign_keys = OFF;
         DROP TABLE permission_http_rule;
         DROP TABLE permission_http_request;
         DROP TABLE permission_request;
         CREATE TABLE permission_request (
             id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL,
             agent TEXT,
             action TEXT NOT NULL,
             resources TEXT NOT NULL,
             save TEXT,
             metadata TEXT,
             source TEXT,
             status TEXT DEFAULT 'pending' NOT NULL,
             reply TEXT,
             suspended_reason TEXT,
             waiter_epoch TEXT NOT NULL,
             time_created INTEGER NOT NULL,
             time_updated INTEGER NOT NULL
         );
         INSERT INTO permission_request VALUES
           ('per_r6_rule', 'ses_r6_permission', 'agt_r6_permission', 'egress',
            '[\"api.example.test\"]', '[\"*.example.test\"]', NULL, NULL,
            'replied', 'always', NULL, 'epoch-r6', 41, 42),
           ('per_r6_pending', 'ses_r6_permission', 'agt_r6_permission', 'write',
            '[\"/work/exact\"]', '[\"/work/*\"]',
            '{\"reason\":\"R6 survival witness\"}',
            '{\"type\":\"tool\",\"messageID\":\"msg-r6-permission\",\"callID\":\"call-r6-permission\"}',
            'pending', NULL, NULL, 'epoch-r6', 43, 43);
         CREATE TABLE rust_migration (
             id TEXT PRIMARY KEY,
             time_completed INTEGER NOT NULL
         );
         INSERT INTO rust_migration VALUES
           ('20260811_rebuild_event_tables_family_tagged', 1),
           ('20260811_create_rust_era_tables', 2),
           ('20260813_create_session_list_projection', 3);",
    )?;
    let source_before = fingerprint_file(&source)?;

    let receipt = rehearse(
        &source,
        &run,
        MigrationId::new("r6mr-permission-survival").ok_or("migration id")?,
        "0.2.0",
        &FixedClock,
    )?;
    assert_eq!(fingerprint_file(&source)?, source_before);

    let mut candidate = open_store(receipt.candidate(), &session)?;
    let rebuilt = PermissionHttpState::default();
    let rows = rebuilt.list(&candidate, &owner, Some(&session))?;
    assert_eq!(rows.requests.len(), 1);
    assert_eq!(rows.requests[0].id, pending);
    assert_eq!(rows.requests[0].agent, agent);
    assert_eq!(rows.requests[0].resources, vec!["/work/exact"]);
    assert_eq!(rows.requests[0].save, Some(vec!["/work/*".to_owned()]));
    assert_eq!(
        rows.requests[0].metadata,
        Some(serde_json::json!({"reason": "R6 survival witness"}))
    );
    assert_eq!(
        rows.requests[0].source,
        Some(PermissionHttpSource::Tool {
            message_id: "msg-r6-permission".to_owned(),
            call_id: "call-r6-permission".to_owned(),
        })
    );

    let export = grapt_store::export_canonical(candidate.connection())?;
    let replay_rows = |table: &str| {
        export
            .event_replayable
            .iter()
            .find(|digest| digest.table == table)
            .map(|digest| digest.rows)
    };
    assert_eq!(replay_rows("permission_request"), Some(2));
    assert_eq!(replay_rows("permission_http_request"), Some(2));
    assert_eq!(replay_rows("permission_http_rule"), Some(1));

    let allowed: PermissionId = "per_r6_allowed".parse()?;
    assert_eq!(
        PermissionHttpState::default().create(
            &mut candidate,
            &owner,
            &event("evt_r6_allowed")?,
            &allowed,
            &session,
            &agent,
            &[],
            &project,
            "egress",
            &["*.example.test".to_owned()],
            None,
            None,
            None,
            44,
        )?,
        PermissionCreateEffect::Allow,
        "the migrated candidate must retain the exact replayed Always resource"
    );

    assert_eq!(
        PermissionHttpState::default().preview_tool(
            &candidate,
            &session,
            &[],
            &project,
            "egress",
            &["cdn.example.test".to_owned()],
            None,
        )?,
        PermissionCreateEffect::Ask,
        "migration must not reinterpret legacy approval text as a pattern"
    );

    let rollback = rusqlite::Connection::open_with_flags(
        receipt.rollback().path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let permission_tables: i64 = rollback.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name IN
           ('permission_request', 'permission_http_request', 'permission_http_rule')",
        [],
        |row| row.get(0),
    )?;
    let permission_events: i64 = rollback.query_row(
        "SELECT COUNT(*) FROM event WHERE type LIKE 'permission.%'",
        [],
        |row| row.get(0),
    )?;
    let legacy_resource_column: i64 = rollback.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('permission_request')
         WHERE name = 'resources' AND [notnull] = 1",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(permission_tables, 1);
    assert_eq!(legacy_resource_column, 1);
    assert_eq!(permission_events, 3);
    assert_eq!(
        &fingerprint_file(receipt.rollback().path())?.1,
        receipt.rollback().file_fingerprint()
    );
    drop(rollback);
    drop(candidate);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

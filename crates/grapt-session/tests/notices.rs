use grapt_session::notice::{self, Unavailable, MAX_NOTICE_TEXT_BYTES};
use grapt_session::{create_session, SessionSeed};
use grapt_store::EventStore;
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, ExecutionId, IdAuthority, IdToken, Timestamp,
};
use std::cell::Cell;

#[derive(Debug, Default)]
struct Authorities(Cell<u64>);
impl ClockAuthority for Authorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1234)
    }
}
impl IdAuthority for Authorities {
    fn mint(&self) -> IdToken {
        self.0.set(self.0.get() + 1);
        IdToken::new(format!("noticewitness{}", self.0.get()))
            .unwrap_or_else(|| panic!("fixture ID refused"))
    }
}

#[test]
fn a_notice_is_recorded_only_when_the_unavailable_set_changes_and_survives_reopening(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "grapt-notice-witness-{}.sqlite",
        std::process::id()
    ));
    assert!(
        !path.exists(),
        "refusing to overwrite existing witness data"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "notice-witness")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_notice").ok_or("project ID refused")?,
        "/work/notice",
        "notice",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let turn = |n: u32| ExecutionId::new(format!("exe_notice{n}")).ok_or("execution ID refused");
    let broken = vec![
        Unavailable::new("notes", "it closed the channel"),
        Unavailable::new("config", "MCP declarations must be an object"),
    ];

    // Nothing was ever unavailable: an empty set stays silent.
    assert_eq!(
        notice::record_mcp_unavailable(&mut store, &authorities, &session, &turn(1)?, vec![])?,
        None
    );
    let first = notice::record_mcp_unavailable(
        &mut store,
        &authorities,
        &session,
        &turn(2)?,
        broken.clone(),
    )?
    .ok_or("a new breakage was not recorded")?;
    // Recorded sorted, so the same set in another order is the same set.
    assert_eq!(first.unavailable[0].source, "config");
    let mut reordered = broken.clone();
    reordered.reverse();
    assert_eq!(
        notice::record_mcp_unavailable(&mut store, &authorities, &session, &turn(3)?, reordered)?,
        None,
        "an unchanged breakage was announced twice"
    );
    drop(store);

    // The comparison is with the durable log, not this process.
    let mut store = EventStore::open(&path, "notice-witness")?;
    store.elect_writer(&grapt_store::AggregateId::Session(session.clone()))?;
    assert_eq!(
        notice::record_mcp_unavailable(
            &mut store,
            &authorities,
            &session,
            &turn(4)?,
            broken.clone()
        )?,
        None,
        "a reopened store announced an unchanged breakage again"
    );
    let cleared =
        notice::record_mcp_unavailable(&mut store, &authorities, &session, &turn(5)?, vec![])?
            .ok_or("the fix was not announced")?;
    assert!(cleared.unavailable.is_empty());
    assert_eq!(
        notice::record_mcp_unavailable(&mut store, &authorities, &session, &turn(6)?, vec![])?,
        None,
        "a cleared set was announced twice"
    );
    assert_eq!(
        notice::notices(store.connection(), &session)?,
        vec![first, cleared]
    );

    // Empty parts are refused; oversized ones are cut on a char boundary.
    assert!(notice::record_mcp_unavailable(
        &mut store,
        &authorities,
        &session,
        &turn(7)?,
        vec![Unavailable::new("", "reason")]
    )
    .is_err());
    let long = Unavailable::new("x", &"é".repeat(MAX_NOTICE_TEXT_BYTES));
    assert!(long.reason.len() <= MAX_NOTICE_TEXT_BYTES);
    assert!(long.reason.chars().all(|c| c == 'é'));
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}

#[test]
fn the_largest_notice_any_input_can_build_is_written_and_read_back(
) -> Result<(), Box<dyn std::error::Error>> {
    let path =
        std::env::temp_dir().join(format!("grapt-notice-worst-{}.sqlite", std::process::id()));
    assert!(
        !path.exists(),
        "refusing to overwrite existing witness data"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "notice-worst")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_worst").ok_or("project ID refused")?,
        "/work/worst",
        "worst",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    // Built directly, past every bound: names of control bytes (six-fold
    // when escaped) and reasons of quotes (two-fold), 64 distinct sources.
    let worst: Vec<Unavailable> = (0..64)
        .map(|n| Unavailable {
            source: format!("{n:02}{}", "\u{1}".repeat(300)),
            reason: "\"".repeat(300),
        })
        .collect();
    let execution = ExecutionId::new("exe_worst").ok_or("execution ID refused")?;
    let recorded =
        notice::record_mcp_unavailable(&mut store, &authorities, &session, &execution, worst)?
            .ok_or("the notice was not recorded")?;
    let read = notice::notices(store.connection(), &session)?;
    drop(store);
    std::fs::remove_file(&path)?;
    assert_eq!(
        read,
        vec![recorded.clone()],
        "the written notice did not read back"
    );
    assert_eq!(recorded.unavailable.len(), 64);
    for entry in &recorded.unavailable {
        assert!(
            entry.source.len() <= MAX_NOTICE_TEXT_BYTES
                && entry.reason.len() <= MAX_NOTICE_TEXT_BYTES
        );
        assert!(
            !entry.source.chars().any(char::is_control),
            "a control byte was stored"
        );
    }
    Ok(())
}

/// Append one raw notice row, as a later build would write it.
fn append_raw_notice(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &grapt_types::SessionId,
    data: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let aggregate = grapt_store::AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let tx = store.governed_transaction()?;
    grapt_session::SessionStore::new(&tx).record(session)?;
    let event: grapt_types::EventId = format!("evt_{}", authorities.mint().as_str()).parse()?;
    grapt_store::EventStore::append_within(
        &tx,
        grapt_store::AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.notice.recorded",
            version: grapt_types::EventVersion::new(1).ok_or("version refused")?,
            created_ms: 1234,
            data: &data,
        },
        &mut [],
    )?;
    tx.commit()?.discard();
    Ok(())
}

/// A notice of a kind a later build wrote is
/// skipped, counted by kind, and never hides the Session's known notices,
/// including from the change detection; a row with no kind still refuses.
#[test]
fn an_unknown_notice_kind_is_skipped_and_counted_not_refused(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "grapt-notice-unknown-{}.sqlite",
        std::process::id()
    ));
    assert!(
        !path.exists(),
        "refusing to overwrite existing witness data"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "notice-unknown")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_unknown").ok_or("project ID refused")?,
        "/work/unknown",
        "unknown",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let execution = ExecutionId::new("exe_unknown").ok_or("execution ID refused")?;
    let broken = vec![Unavailable::new("notes", "it closed the channel")];
    let known = notice::record_mcp_unavailable(
        &mut store,
        &authorities,
        &session,
        &execution,
        broken.clone(),
    )?
    .ok_or("the notice was not recorded")?;
    // Newer than the known one, so a LIMIT-1 read would land on it.
    append_raw_notice(
        &mut store,
        &authorities,
        &session,
        serde_json::json!({
            "sessionID": session.as_str(), "executionID": "exe_unknown",
            "kind": "a-later-kind", "reason": "written by a later build",
        }),
    )?;
    let notice::NoticeRead {
        mcp: read, skipped, ..
    } = notice::read_notices(store.connection(), &session)?;
    assert_eq!(read, vec![known.clone()], "the known notice was hidden");
    assert_eq!(
        skipped,
        vec![notice::SkippedKind {
            kind: "a-later-kind".to_owned(),
            count: 1
        }]
    );
    assert_eq!(notice::notices(store.connection(), &session)?, vec![known]);
    assert_eq!(
        notice::record_mcp_unavailable(&mut store, &authorities, &session, &execution, broken)?,
        None,
        "an unknown kind made an unchanged breakage look new"
    );
    // No kind at all is malformed, not unknown.
    append_raw_notice(
        &mut store,
        &authorities,
        &session,
        serde_json::json!({"sessionID": session.as_str(), "executionID": "exe_unknown"}),
    )?;
    assert!(
        notice::notices(store.connection(), &session).is_err(),
        "a kindless notice was accepted"
    );
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}

/// Only a string kind can be unknown, so
/// a kind of any other JSON type still refuses the read; and a skipped
/// kind's name is bounded in bytes, cut on a character boundary.
#[test]
fn only_a_string_kind_is_skippable_and_its_name_is_byte_bounded(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "grapt-notice-kind-shapes-{}.sqlite",
        std::process::id()
    ));
    assert!(
        !path.exists(),
        "refusing to overwrite existing witness data"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "notice-kind-shapes")?;
    let fresh = |store: &mut EventStore, name: &str| -> Result<_, Box<dyn std::error::Error>> {
        let seed = SessionSeed::new(
            grapt_types::ProjectId::new(format!("prj_{name}")).ok_or("project ID refused")?,
            format!("/work/{name}"),
            name,
        )?;
        Ok(create_session(store, &authorities, &seed, &mut [])?.session)
    };
    for (name, kind) in [
        ("number", serde_json::json!(7)),
        ("boolean", serde_json::json!(true)),
        ("array", serde_json::json!([])),
        ("object", serde_json::json!({})),
    ] {
        let session = fresh(&mut store, name)?;
        append_raw_notice(
            &mut store,
            &authorities,
            &session,
            serde_json::json!({"sessionID": session.as_str(), "executionID": "exe_shape", "kind": kind}),
        )?;
        assert!(
            notice::notices(store.connection(), &session).is_err(),
            "a {name} kind was skipped instead of refused"
        );
    }
    let session = fresh(&mut store, "emoji")?;
    let wide = "😀".repeat(70);
    append_raw_notice(
        &mut store,
        &authorities,
        &session,
        serde_json::json!({"sessionID": session.as_str(), "executionID": "exe_wide", "kind": wide}),
    )?;
    let notice::NoticeRead {
        mcp: read, skipped, ..
    } = notice::read_notices(store.connection(), &session)?;
    assert!(read.is_empty());
    assert_eq!(skipped.len(), 1);
    assert!(
        skipped[0].kind.len() <= notice::MAX_SKIPPED_KIND_BYTES,
        "{} bytes",
        skipped[0].kind.len()
    );
    assert!(
        wide.starts_with(&skipped[0].kind),
        "the cut split a character"
    );
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}

/// A not-run-yet notice is recorded for every
/// blocked prompt, reads back beside the MCP notices, is never a skipped
/// kind, and never disturbs the MCP change detection.
#[test]
fn a_not_run_yet_notice_reads_beside_mcp_notices() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "grapt-notice-not-run-yet-{}.sqlite",
        std::process::id()
    ));
    assert!(
        !path.exists(),
        "refusing to overwrite existing witness data"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "notice-not-run-yet")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_queued").ok_or("project ID refused")?,
        "/work/queued",
        "queued",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let first = ExecutionId::new("exe_queued1").ok_or("execution ID refused")?;
    let second = ExecutionId::new("exe_queued2").ok_or("execution ID refused")?;
    let broken = vec![Unavailable::new("notes", "it closed the channel")];
    let known =
        notice::record_mcp_unavailable(&mut store, &authorities, &session, &first, broken.clone())?
            .ok_or("the MCP notice was not recorded")?;
    let blocked = notice::record_not_run_yet(
        &mut store,
        &authorities,
        &session,
        &first,
        &"msg_queued1".parse().map_err(|_| "message ID refused")?,
        "catalog configuration is not a regular file: /work/grapt.json",
    )?;
    // The same block again is told again: every waiting prompt hears it.
    let again = notice::record_not_run_yet(
        &mut store,
        &authorities,
        &session,
        &second,
        &"msg_queued2".parse().map_err(|_| "message ID refused")?,
        "catalog configuration is not a regular file: /work/grapt.json",
    )?;
    let queued: grapt_types::MessageId = "msg_queued2".parse().map_err(|_| "message ID refused")?;
    assert!(
        notice::record_not_run_yet(&mut store, &authorities, &session, &second, &queued, "")
            .is_err()
    );
    let read = notice::read_notices(store.connection(), &session)?;
    assert_eq!(read.mcp, vec![known]);
    assert_eq!(read.not_run_yet, vec![blocked, again]);
    assert_eq!(read.not_run_yet[0].message.as_str(), "msg_queued1");
    assert!(read.skipped.is_empty(), "a known kind was skipped");
    assert_eq!(
        notice::record_mcp_unavailable(&mut store, &authorities, &session, &second, broken)?,
        None,
        "a not-run-yet notice made an unchanged MCP breakage look new"
    );
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}

/// A context-repeated notice reads back beside the other
/// kinds, and a build that predates the kind, with its
/// tolerant reader, still folds the Session: its MCP and not-run-yet readers
/// pass over the row, and its skipped-kinds query counts it rather than
/// refusing.
#[test]
fn a_context_repeated_notice_reads_back_and_an_older_reader_skips_it(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "grapt-notice-context-repeated-{}.sqlite",
        std::process::id()
    ));
    assert!(
        !path.exists(),
        "refusing to overwrite existing witness data"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "notice-context-repeated")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_repeated").ok_or("project ID refused")?,
        "/work/repeated",
        "repeated",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let execution = ExecutionId::new("exe_repeated").ok_or("execution ID refused")?;
    let broken = vec![Unavailable::new("notes", "it closed the channel")];
    let known =
        notice::record_mcp_unavailable(&mut store, &authorities, &session, &execution, broken)?
            .ok_or("the MCP notice was not recorded")?;
    let repeated = notice::record_context_repeated(&mut store, &authorities, &session, &execution)?;

    let read = notice::read_notices(store.connection(), &session)?;
    assert_eq!(read.context_repeated, vec![repeated]);
    assert_eq!(read.mcp, vec![known.clone()]);
    assert!(read.skipped.is_empty(), "a known kind was skipped");

    // The older build's readers: the same MCP and not-run-yet readers...
    assert_eq!(notice::notices(store.connection(), &session)?, vec![known]);
    assert!(notice::not_run_yet_notices(store.connection(), &session)?.is_empty());
    // ...and its skipped-kinds query, verbatim from the older build, which
    // knew only these two kinds: the row is a string kind, so it is counted.
    let counted: Vec<(String, i64)> = store
        .connection()
        .prepare(
            "SELECT substr(kind, 1, ?3), COUNT(*) FROM (
               SELECT json_extract(data, '$.kind') AS kind, json_type(data, '$.kind') AS shape
               FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.notice.recorded.1')
             WHERE shape = 'text' AND kind NOT IN (?4, ?5)
             GROUP BY kind ORDER BY kind LIMIT ?2",
        )?
        .query_map(
            (session.as_str(), 8, 64, "mcp-unavailable", "not-run-yet"),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<Result<_, _>>()?;
    assert_eq!(counted, vec![("context-repeated".to_owned(), 1)]);
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}

/// A config key SE accepts but does not apply is named once per Session per
/// key, however many executions read the same configuration.
#[test]
fn an_ignored_config_key_is_named_once_per_session() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "grapt-notice-config-ignored-{}.sqlite",
        std::process::id()
    ));
    assert!(
        !path.exists(),
        "refusing to overwrite existing witness data"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "notice-config-ignored")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_ignored").ok_or("project ID refused")?,
        "/work/ignored",
        "ignored",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let first = ExecutionId::new("exe_ignored1").ok_or("execution ID refused")?;
    let second = ExecutionId::new("exe_ignored2").ok_or("execution ID refused")?;
    let reason = "SE uses its own recent-context retention";
    let named = notice::record_config_ignored(
        &mut store,
        &authorities,
        &session,
        &first,
        "compaction.keep.tokens",
        reason,
    )?
    .ok_or("the ignored key was not named")?;
    assert_eq!(
        notice::record_config_ignored(
            &mut store,
            &authorities,
            &session,
            &second,
            "compaction.keep.tokens",
            reason,
        )?,
        None,
        "the same key was named twice"
    );
    let other = notice::record_config_ignored(
        &mut store,
        &authorities,
        &session,
        &second,
        "some.other.key",
        reason,
    )?
    .ok_or("another key was not named")?;
    assert!(
        notice::record_config_ignored(&mut store, &authorities, &session, &second, "", reason)
            .is_err()
    );
    let read = notice::read_notices(store.connection(), &session)?;
    assert_eq!(read.config_ignored, vec![named, other]);
    assert_eq!(read.config_ignored[0].key, "compaction.keep.tokens");
    assert!(read.mcp.is_empty() && read.not_run_yet.is_empty());
    assert!(read.skipped.is_empty(), "a known kind was skipped");

    // Revert safety: a build that predates the kind passes over the
    // rows, and its skipped-kinds query, which knew only these two
    // kinds, counts them.
    assert!(notice::notices(store.connection(), &session)?.is_empty());
    assert!(notice::not_run_yet_notices(store.connection(), &session)?.is_empty());
    let counted: Vec<(String, i64)> = store
        .connection()
        .prepare(
            "SELECT substr(kind, 1, ?3), COUNT(*) FROM (
               SELECT json_extract(data, '$.kind') AS kind, json_type(data, '$.kind') AS shape
               FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.notice.recorded.1')
             WHERE shape = 'text' AND kind NOT IN (?4, ?5)
             GROUP BY kind ORDER BY kind LIMIT ?2",
        )?
        .query_map(
            (session.as_str(), 8, 64, "mcp-unavailable", "not-run-yet"),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<Result<_, _>>()?;
    assert_eq!(counted, vec![("config-ignored".to_owned(), 2)]);
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}

/// The "already named" check reads the prior notice for the key through
/// the validating reader (notice_probe.rs). A corrupt row (no executionID)
/// refuses instead of silencing the notice; a valid prior row still dedups.
#[test]
fn a_corrupt_prior_config_notice_refuses_instead_of_deduping(
) -> Result<(), Box<dyn std::error::Error>> {
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_dedup").ok_or("project ID refused")?,
        "/work/dedup",
        "dedup",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let execution = ExecutionId::new("exe_dedup").ok_or("execution ID refused")?;
    let reason = "ignored";

    // A valid prior row, written raw, still dedups.
    append_raw_notice(
        &mut store,
        &authorities,
        &session,
        serde_json::json!({"sessionID": session.as_str(), "executionID": "exe_prior",
            "kind": "config-ignored", "key": "compaction.keep.tokens", "reason": reason}),
    )?;
    assert_eq!(
        notice::record_config_ignored(
            &mut store,
            &authorities,
            &session,
            &execution,
            "compaction.keep.tokens",
            reason
        )?,
        None,
        "a valid prior notice did not dedup"
    );

    // The probe's row: the same kind and key, no executionID.
    append_raw_notice(
        &mut store,
        &authorities,
        &session,
        serde_json::json!({"sessionID": session.as_str(), "kind": "config-ignored",
            "key": "compaction.reserve", "reason": reason}),
    )?;
    let refused = notice::record_config_ignored(
        &mut store,
        &authorities,
        &session,
        &execution,
        "compaction.reserve",
        reason,
    );
    assert!(
        refused.is_err(),
        "a corrupt prior notice silenced the notice: {refused:?}"
    );
    // A row that decodes but fails validation (an empty reason) refuses too.
    append_raw_notice(
        &mut store,
        &authorities,
        &session,
        serde_json::json!({"sessionID": session.as_str(), "executionID": "exe_prior",
            "kind": "config-ignored", "key": "compaction.auto", "reason": ""}),
    )?;
    assert!(notice::record_config_ignored(
        &mut store,
        &authorities,
        &session,
        &execution,
        "compaction.auto",
        reason
    )
    .is_err());
    // The reader refuses the same rows, so the two agree.
    assert!(notice::config_ignored_notices(store.connection(), &session).is_err());
    // A key the corrupt row does not name is still named.
    assert!(notice::record_config_ignored(
        &mut store,
        &authorities,
        &session,
        &execution,
        "some.other.key",
        reason
    )?
    .is_some());
    Ok(())
}

/// An execution that stopped on an unusable summary
/// leaves a compaction-failed notice. It reads back as its own kind, never
/// as a context-repeated one, and a reverted build
/// skips and counts it rather than refusing the Session's notices.
#[test]
fn a_compaction_failed_notice_reads_back_and_a_reverted_reader_skips_it(
) -> Result<(), Box<dyn std::error::Error>> {
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_unusable").ok_or("project ID refused")?,
        "/work/unusable",
        "unusable",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let execution = ExecutionId::new("exe_unusable").ok_or("execution ID refused")?;
    let repeated = notice::record_context_repeated(&mut store, &authorities, &session, &execution)?;
    let failed = notice::record_compaction_failed(&mut store, &authorities, &session, &execution)?;
    assert_eq!(failed.execution, execution);

    let read = notice::read_notices(store.connection(), &session)?;
    assert_eq!(read.compaction_failed, vec![failed]);
    assert_eq!(read.context_repeated, vec![repeated.clone()]);
    assert!(read.skipped.is_empty(), "a known kind was skipped");

    // The reverted build's readers: its context-repeated reader does not
    // take the row for its own kind...
    assert_eq!(
        notice::context_repeated_notices(store.connection(), &session)?,
        vec![repeated]
    );
    // ...and its skipped-kinds query, verbatim from the reverted build,
    // which knew four kinds: the row is a string kind, so it is counted,
    // not refused.
    let counted: Vec<(String, i64)> = store
        .connection()
        .prepare(
            "SELECT substr(kind, 1, ?3), COUNT(*) FROM (
               SELECT json_extract(data, '$.kind') AS kind, json_type(data, '$.kind') AS shape
               FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type = 'session.notice.recorded.1')
             WHERE shape = 'text' AND kind NOT IN (?4, ?5, ?6, ?7)
             GROUP BY kind ORDER BY kind LIMIT ?2",
        )?
        .query_map(
            (
                session.as_str(),
                8,
                64,
                "mcp-unavailable",
                "not-run-yet",
                "context-repeated",
                "config-ignored",
            ),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<Result<_, _>>()?;
    assert_eq!(counted, vec![("compaction-failed".to_owned(), 1)]);

    // A row of the kind naming another Session refuses.
    append_raw_notice(
        &mut store,
        &authorities,
        &session,
        serde_json::json!({"sessionID": "ses_someoneelse", "executionID": "exe_x",
            "kind": "compaction-failed"}),
    )?;
    assert!(notice::compaction_failed_notices(store.connection(), &session).is_err());
    Ok(())
}

/// A retried execution is many attempts of one execution, so the hint is true
/// once for the execution however many attempts fail.
#[test]
fn the_enable_hint_is_rendered_once_per_execution_across_retries(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_types::PhysicalAttemptId;
    use notice::ProviderErrorOutcome;
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_error_hint").ok_or("project")?,
        "/work",
        "error hint",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let (execution, later) = (
        ExecutionId::new("exe_rate_limited").ok_or("execution")?,
        ExecutionId::new("exe_later").ok_or("execution")?,
    );
    for index in 0..6 {
        let owner = if index == 5 { &later } else { &execution };
        let recorded = notice::record_provider_error_detail(
            &mut store,
            &authorities,
            &session,
            owner,
            &PhysicalAttemptId::new(format!("att_{index}")).ok_or("attempt")?,
            ProviderErrorOutcome::EnableHint,
        )?;
        assert_eq!(recorded.is_some(), index % 5 == 0, "attempt {index}");
    }
    assert_eq!(
        notice::read_notices(store.connection(), &session)?
            .provider_error_detail
            .len(),
        2
    );
    Ok(())
}

#[test]
fn an_enable_hint_cannot_reassign_an_attempt_to_another_execution(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_types::PhysicalAttemptId;
    use notice::ProviderErrorOutcome;
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_hint_owner").ok_or("project")?,
        "/work",
        "hint owner",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let attempt = PhysicalAttemptId::new("att_owned").ok_or("attempt")?;
    let (first, other) = (
        ExecutionId::new("exe_first").ok_or("execution")?,
        ExecutionId::new("exe_other").ok_or("execution")?,
    );
    assert!(notice::record_provider_error_detail(
        &mut store,
        &authorities,
        &session,
        &first,
        &attempt,
        ProviderErrorOutcome::EnableHint,
    )?
    .is_some());
    assert!(
        notice::record_provider_error_detail(
            &mut store,
            &authorities,
            &session,
            &other,
            &attempt,
            ProviderErrorOutcome::EnableHint,
        )
        .is_err(),
        "an existing physical attempt changed execution owner"
    );
    Ok(())
}

#[test]
fn hint_scope_survives_reopen_and_keeps_prior_row_validation(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_types::PhysicalAttemptId;
    use notice::ProviderErrorOutcome;
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let path =
        std::env::temp_dir().join(format!("grapt-hint-reopen-{}.sqlite", std::process::id()));
    assert!(!path.exists(), "refusing to overwrite fixture data");
    let mut store = EventStore::open(&path, "hint-reopen")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_hint_reopen").ok_or("project")?,
        "/work",
        "hint reopening",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let execution = ExecutionId::new("exe_hint_reopen").ok_or("execution")?;
    assert!(notice::record_provider_error_detail(
        &mut store,
        &authorities,
        &session,
        &execution,
        &PhysicalAttemptId::new("att_before_reopen").ok_or("attempt")?,
        ProviderErrorOutcome::EnableHint,
    )?
    .is_some());
    drop(store);
    let mut store = EventStore::open(&path, "hint-reopen")?;
    store.elect_writer(&grapt_store::AggregateId::Session(session.clone()))?;
    for (id, outcome, expected) in [
        ("att_after_reopen", ProviderErrorOutcome::EnableHint, false),
        ("att_saved", ProviderErrorOutcome::Saved, true),
        ("att_unavailable", ProviderErrorOutcome::Unavailable, true),
    ] {
        assert_eq!(
            notice::record_provider_error_detail(
                &mut store,
                &authorities,
                &session,
                &execution,
                &PhysicalAttemptId::new(id).ok_or("attempt")?,
                outcome,
            )?
            .is_some(),
            expected
        );
    }
    assert_eq!(
        notice::read_notices(store.connection(), &session)?
            .provider_error_detail
            .len(),
        3
    );
    rusqlite::Connection::open(&path)?.execute(
        "UPDATE event SET created=-1 WHERE json_extract(data,'$.attemptID')='att_unavailable'",
        [],
    )?;
    assert_eq!(
        notice::record_provider_error_detail(
            &mut store,
            &authorities,
            &session,
            &execution,
            &PhysicalAttemptId::new("att_bad_time").ok_or("attempt")?,
            ProviderErrorOutcome::EnableHint,
        )
        .err()
        .as_deref(),
        Some("notice time is negative")
    );
    drop(store);
    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn provider_error_notices_are_fixed_deduplicated_and_retry_attributed(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_types::PhysicalAttemptId;
    use notice::ProviderErrorOutcome;
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let path =
        std::env::temp_dir().join(format!("grapt-error-notice-{}.sqlite", std::process::id()));
    assert!(!path.exists(), "refusing to overwrite witness data");
    let mut store = EventStore::open(&path, "error-detail-witness")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_error_detail").ok_or("project")?,
        "/work",
        "error detail",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let execution = ExecutionId::new("exe_error_detail").ok_or("execution")?;
    for (id, outcome) in [
        ("att_first", ProviderErrorOutcome::Saved),
        ("att_retry", ProviderErrorOutcome::Unavailable),
    ] {
        let attempt = PhysicalAttemptId::new(id).ok_or("attempt")?;
        assert!(notice::record_provider_error_detail(
            &mut store,
            &authorities,
            &session,
            &execution,
            &attempt,
            outcome
        )?
        .is_some());
        assert!(notice::record_provider_error_detail(
            &mut store,
            &authorities,
            &session,
            &execution,
            &attempt,
            ProviderErrorOutcome::EnableHint
        )?
        .is_none());
    }
    let read = notice::read_notices(store.connection(), &session)?;
    assert!(read.skipped.is_empty());
    assert_eq!(read.provider_error_detail.len(), 2);
    assert_eq!(read.provider_error_detail[0].attempt.as_str(), "att_first");
    assert_eq!(read.provider_error_detail[1].attempt.as_str(), "att_retry");
    assert_eq!(
        read.provider_error_detail[0].outcome,
        ProviderErrorOutcome::Saved
    );
    assert_eq!(
        read.provider_error_detail[1].outcome,
        ProviderErrorOutcome::Unavailable
    );
    assert!(notice::record_provider_error_detail(
        &mut store,
        &authorities,
        &session,
        &ExecutionId::new("exe_other").ok_or("other execution")?,
        &PhysicalAttemptId::new("att_first").ok_or("attempt")?,
        ProviderErrorOutcome::Saved
    )
    .is_err());
    drop(store);
    let mut store = EventStore::open(&path, "error-detail-witness")?;
    store.elect_writer(&grapt_store::AggregateId::Session(session.clone()))?;
    assert!(notice::record_provider_error_detail(
        &mut store,
        &authorities,
        &session,
        &execution,
        &PhysicalAttemptId::new("att_first").ok_or("attempt")?,
        ProviderErrorOutcome::Saved
    )?
    .is_none());
    append_raw_notice(
        &mut store,
        &authorities,
        &session,
        serde_json::json!({
            "sessionID":session.as_str(),"executionID":execution.as_str(),"attemptID":"att_bad",
            "kind":"provider-error-detail","reason":"vendor text that must not be accepted",
        }),
    )?;
    assert!(notice::read_notices(store.connection(), &session).is_err());
    drop(store);
    std::fs::remove_file(path)?;
    Ok(())
}

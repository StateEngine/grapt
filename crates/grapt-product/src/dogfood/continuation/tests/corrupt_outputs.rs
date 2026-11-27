//! Unreadable tool terminals and outputs degrade instead of refusing: a
//! corrupt output becomes a failed output with a notice, never a parsed receipt.
use super::recent::raw_conn;
use super::*;

/// Overwrites the one tool terminal's artifact so its digest no longer verifies.
pub(super) fn corrupt_terminal_artifacts(fixture: &Fixture) -> Result<(), String> {
    std::fs::write(terminal_object(fixture)?, b"corrupt output").map_err(|error| error.to_string())
}

/// The one tool terminal's artifact object file.
fn terminal_object(fixture: &Fixture) -> Result<std::path::PathBuf, String> {
    let id: String = raw_conn(fixture)?
        .query_row(
            "SELECT COALESCE(json_extract(data, '$.outcome.output.id'),
                             json_extract(data, '$.outcome.detail.id'))
             FROM event WHERE type = 'session.tool.terminal.1'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let shard = id.get(..2).ok_or("artifact identity had no shard")?;
    Ok(fixture.artifact_root.join("objects").join(shard).join(&id))
}

/// A non-JSON or undecodable terminal elsewhere in the Session neither hides a
/// readable terminal's position nor declares bytes.
#[test]
fn foreign_unreadable_terminals_leave_positions_and_declared_bytes_alone() -> Result<(), String> {
    for data in [
        r#"'{'"#,
        r#"'{"attempt":1,"call":1,"outcome":{"output":{"length":4000000000}}}'"#,
        // Text identities naming the real call, but no decodable terminal.
        r#"'{"attempt":"x","call":"call_d2a_read","outcome":{"kind":"completed","output":{"length":4000000000}}}'"#,
        // Text that is not UTF-8, and a row past the stored-terminal bound.
        r#"CAST(X'7B2263616C6C223A22FF227D' AS TEXT)"#,
        r#"'{"call":"call_d2a_read","pad":"' || printf('%.*c', 70000, 'x') || '"}'"#,
    ] {
        let fixture = Fixture::new("unreadable-terminal-reads")?;
        fixture.record_exchange()?;
        super::sustained_defects::append_malformed_session_event(
            &fixture,
            "evt_unreadable",
            "session.tool.terminal",
            1,
        )?;
        raw_conn(&fixture)?
            .execute(
                &format!("UPDATE event SET data = {data} WHERE id = 'evt_unreadable'"),
                [],
            )
            .map_err(|error| error.to_string())?;
        let inbox = lock(fixture.inbox()?);
        let shown = std::collections::BTreeSet::new();
        let notices = super::unknown_effects::notices(
            inbox.store(),
            &fixture.session,
            u64::MAX >> 1,
            &shown,
        )?;
        assert_eq!(notices.len(), 1, "{data}");
        let call = ToolCallId::new("call_d2a_read").ok_or("call")?;
        grapt_store::event_publication::tool_position(
            inbox.store(),
            &fixture.session,
            &call,
            "session.tool.terminal.1",
            None,
            "$.call",
        )?;
        let over = grapt_store::session_event_queries::boundary::declared_bytes_over(
            inbox.store(),
            &fixture.session,
            None,
            None,
            1_000_000,
            1_000_000,
            None,
        )?;
        assert_eq!(over, None, "{data}");
    }
    Ok(())
}

#[test]
fn corrupt_outputs_and_receipts_degrade_in_history_and_the_next_request() -> Result<(), String> {
    for (tool, action) in [
        (READ_TOOL, "read"),
        (crate::dogfood::tools::image::TOOL, "read"),
        (crate::dogfood::tools::EDIT_TOOL, "edit"),
    ] {
        let fixture = Fixture::new("corrupt-receipt")?;
        fixture.record_exchange_output(true, false, false, tool, action, b"receipt")?;
        corrupt_terminal_artifacts(&fixture)?;
        let call = ToolCallId::new("call_d2a_read").ok_or("call")?;
        let projection = tool_projection(
            fixture.inbox()?,
            fixture.artifacts()?,
            &fixture.session,
            &call,
        )?;
        assert!(
            projection.error.is_some(),
            "{tool}: corrupt output not failed"
        );
        crate::dogfood::history_json::tool_state_json(&projection, fixture.artifacts()?, true)?;
        {
            // The recent window reads under a byte budget through its own path.
            let inbox = lock(fixture.inbox()?);
            let (store, artifacts) = (inbox.store(), fixture.artifacts()?);
            let window = BoundedWindow {
                from: None,
                until: None,
                max_rows: MAX_CONTINUATION_ROWS,
            };
            let calls = stored_calls_for(store, &fixture.session, artifacts, window, None, None)?;
            let mut room = 1 << 20;
            let budget = Some((&mut room, calls.as_slice()));
            let outputs =
                stored_outputs_for(store, &fixture.session, artifacts, window, None, budget)?;
            assert!(
                outputs.iter().all(|output| output.error.is_some()),
                "{tool}"
            );
        }
        let input = fixture
            .continuation(fixture.input.clone())?
            .request_input("use the tool", true)?;
        let output = input.iter().find_map(|item| match item {
            CodexInputItem::ToolOutput {
                output, is_error, ..
            } => Some((output.as_str(), *is_error)),
            _ => None,
        });
        let (text, is_error) = output.ok_or("the corrupt output left the request")?;
        assert!(is_error, "{tool}");
        assert!(text.contains("unavailable or corrupt"), "{tool}: {text}");
    }
    Ok(())
}

#[test]
fn a_corrupt_nested_machine_output_omits_only_its_files() -> Result<(), String> {
    let fixture = Fixture::new("corrupt-nested")?;
    let parent = nested_child(&fixture, br#"{"value":{},"artifacts":[]}"#)?;
    corrupt_terminal_artifacts(&fixture)?;
    let inbox = lock(fixture.inbox()?);
    let artifacts = fixture.artifacts()?;
    assert!(files::collect(inbox.store(), artifacts, &fixture.session, &parent)?.is_empty());
    Ok(())
}

/// One nested child of a new parent call, completed with `output` as its
/// machine output; returns the parent.
fn nested_child(fixture: &Fixture, output: &[u8]) -> Result<ToolCallId, String> {
    let mut owner = super::sustained_defects::step_owner(fixture, 1)?;
    let (parent, call) = (
        ToolCallId::new("nested-parent").ok_or("parent")?,
        ToolCallId::new("nested-child").ok_or("call")?,
    );
    owner
        .commit(DurableFact::NestedToolCalled {
            parent: parent.clone(),
            call: call.clone(),
            tool: grapt_types::ToolDefinitionId::new("image").ok_or("tool")?,
            catalog: grapt_types::ToolSchemaVersion::new("catalog-1").ok_or("catalog")?,
            input: grapt_runtime::BoundedToolInput::new(b"{}".to_vec())
                .map_err(|error| error.to_string())?,
        })
        .map_err(|error| error.to_string())?;
    let captured = fixture
        .artifacts()?
        .capture(
            Cursor::new(output.to_vec()),
            CaptureBound::new(4096, 4096).ok_or("bound")?,
            ArtifactKind::CapturedOutput,
            Sensitivity::SecretBearing,
        )
        .map_err(|error| error.to_string())?;
    let output = ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
        .ok_or("output")?;
    owner
        .commit(DurableFact::ToolTerminal {
            call,
            outcome: ToolTerminalOutcome::Completed {
                output: Some(output),
            },
        })
        .map_err(|error| error.to_string())?;
    Ok(parent)
}

/// An image output whose first chunk verifies but whose tail does not is a
/// failed output in the recent window; its notice is never parsed as a receipt.
#[test]
fn a_corrupt_image_tail_after_a_verified_first_chunk_degrades() -> Result<(), String> {
    let fixture = Fixture::new("corrupt-image-tail")?;
    let mut receipt = br#"{"kind":"secode-image-v1""#.to_vec();
    receipt.resize(100_000, b' ');
    let tool = crate::dogfood::tools::image::TOOL;
    fixture.record_exchange_output(true, false, false, tool, "read", &receipt)?;
    let object = terminal_object(&fixture)?;
    let mut bytes = std::fs::read(&object).map_err(|error| error.to_string())?;
    let last = bytes.last_mut().ok_or("empty image artifact")?;
    *last ^= 1;
    std::fs::write(&object, bytes).map_err(|error| error.to_string())?;
    let inbox = lock(fixture.inbox()?);
    let (store, artifacts) = (inbox.store(), fixture.artifacts()?);
    let window = BoundedWindow {
        from: None,
        until: None,
        max_rows: MAX_CONTINUATION_ROWS,
    };
    let calls = stored_calls_for(store, &fixture.session, artifacts, window, None, None)?;
    let mut room = 1 << 20;
    let budget = Some((&mut room, calls.as_slice()));
    let outputs = stored_outputs_for(store, &fixture.session, artifacts, window, None, budget)?;
    assert!(outputs.iter().all(|output| output.error.is_some()));
    Ok(())
}

/// A call whose only terminal is unreadable, even past the stored-terminal
/// bound, stays listed and fetches as the notice it lists as.
#[test]
fn a_call_with_only_an_unreadable_terminal_fetches_its_notice() -> Result<(), String> {
    for data in [
        r#"'{"call":"call_d2a_read"}'"#,
        r#"'{"call":"call_d2a_read","pad":"' || printf('%.*c', 70000, 'x') || '"}'"#,
    ] {
        let fixture = Fixture::new("unreadable-terminal-fetch")?;
        fixture.record_exchange()?;
        raw_conn(&fixture)?
            .execute(
                &format!("UPDATE event SET data = {data} WHERE type = 'session.tool.terminal.1'"),
                [],
            )
            .map_err(|error| error.to_string())?;
        let call = ToolCallId::new("call_d2a_read").ok_or("call")?;
        let artifacts = fixture.artifacts()?.as_ref();
        let (inbox, session) = (fixture.inbox()?, &fixture.session);
        let listed = history(inbox, artifacts, session, None, None, None)?;
        assert!(
            String::from_utf8_lossy(&listed).contains("call_d2a_read"),
            "{data}"
        );
        let fetched = history(inbox, artifacts, session, Some(&call), None, None)?;
        assert!(
            String::from_utf8_lossy(&fetched).contains("unreadable"),
            "{data}"
        );
        // The batched page projects the call exactly as its own read does.
        let state = |projection: &ToolProjection| {
            crate::dogfood::history_json::tool_state_json(projection, fixture.artifacts()?, true)
        };
        let own = tool_projection(inbox, fixture.artifacts()?, session, &call)?;
        let mut batched = ToolProjection::read_batches(
            inbox,
            fixture.artifacts()?,
            session,
            std::slice::from_ref(&call),
        );
        let batched = batched.remove(&call).ok_or("batched projection absent")??;
        assert_eq!(state(&batched)?, state(&own)?, "{data}");
    }
    Ok(())
}

/// Malformed nested machine output omits that child's files; collection goes on.
#[test]
fn a_malformed_nested_machine_output_omits_only_its_files() -> Result<(), String> {
    let fixture = Fixture::new("malformed-nested")?;
    let parent = nested_child(&fixture, b"{not json")?;
    let inbox = lock(fixture.inbox()?);
    let artifacts = fixture.artifacts()?;
    assert!(files::collect(inbox.store(), artifacts, &fixture.session, &parent)?.is_empty());
    Ok(())
}

/// A malformed entry in a nested machine output costs only that entry.
#[test]
fn a_malformed_nested_file_entry_keeps_its_siblings() -> Result<(), String> {
    let fixture = Fixture::new("malformed-nested-entry")?;
    let file = fixture
        .artifacts()?
        .capture(
            Cursor::new(b"attached".to_vec()),
            CaptureBound::new(4096, 4096).ok_or("bound")?,
            ArtifactKind::CapturedOutput,
            Sensitivity::SecretBearing,
        )
        .map_err(|error| error.to_string())?;
    let file = file.reference();
    let healthy = serde_json::json!({"id": file.id().as_str(), "digest": file.digest().as_str(),
        "length": file.length(), "mimeType": "text/plain", "sensitivity": "secret-bearing"});
    let output = serde_json::json!({"value": {}, "artifacts": [{}, healthy]}).to_string();
    let parent = nested_child(&fixture, output.as_bytes())?;
    let inbox = lock(fixture.inbox()?);
    let files = files::collect(
        inbox.store(),
        fixture.artifacts()?,
        &fixture.session,
        &parent,
    )?;
    assert_eq!(files.len(), 1);
    Ok(())
}

/// A verified image output that is no receipt is a failed output, in the
/// recent window and the next request alike.
#[test]
fn a_malformed_image_receipt_degrades_to_a_failed_output() -> Result<(), String> {
    // With and without the permission row: an explicit local identity alone
    // proves the image read, so a missing marker is corruption too.
    for unpermitted in [false, true] {
        let fixture = Fixture::new("malformed-image-receipt")?;
        let tool = crate::dogfood::tools::image::TOOL;
        fixture.record_exchange_output(true, false, false, tool, "read", b"not a receipt")?;
        if unpermitted {
            let conn = raw_conn(&fixture)?;
            conn.execute("DELETE FROM event WHERE type LIKE 'permission.%'", [])
                .map_err(|error| error.to_string())?;
        }
        {
            let inbox = lock(fixture.inbox()?);
            let (store, artifacts) = (inbox.store(), fixture.artifacts()?);
            let window = BoundedWindow {
                from: None,
                until: None,
                max_rows: MAX_CONTINUATION_ROWS,
            };
            let calls = stored_calls_for(store, &fixture.session, artifacts, window, None, None)?;
            let mut room = 1 << 20;
            let budget = Some((&mut room, calls.as_slice()));
            let outputs =
                stored_outputs_for(store, &fixture.session, artifacts, window, None, budget)?;
            assert!(outputs.iter().all(|output| output.error.is_some()));
        }
        let call = ToolCallId::new("call_d2a_read").ok_or("call")?;
        let artifacts = fixture.artifacts()?;
        let projection = tool_projection(fixture.inbox()?, artifacts, &fixture.session, &call)?;
        let state = crate::dogfood::history_json::tool_state_json(&projection, artifacts, true)?;
        assert!(state.contains("unavailable or corrupt"), "{state}");
        let input = fixture
            .continuation(fixture.input.clone())?
            .request_input("use the tool", true)?;
        assert!(input.iter().any(|item| matches!(item,
            CodexInputItem::ToolOutput { is_error: true, output, .. }
                if output.as_str().contains("unavailable or corrupt"))));
    }
    Ok(())
}

/// An unreadable terminal's call identity is shown bounded, whatever its length.
#[test]
fn an_unknown_effect_notice_bounds_the_identity_it_names() -> Result<(), String> {
    let fixture = Fixture::new("long-identity-notice")?;
    fixture.record_exchange()?;
    let call = "\u{e9}".repeat(30_000);
    raw_conn(&fixture)?
        .execute(
            "UPDATE event SET data = json_object('call', ?1) WHERE type = 'session.tool.terminal.1'",
            [&call],
        )
        .map_err(|error| error.to_string())?;
    let inbox = lock(fixture.inbox()?);
    let shown = std::collections::BTreeSet::new();
    let notices =
        super::unknown_effects::notices(inbox.store(), &fixture.session, u64::MAX >> 1, &shown)?;
    let [(_, CodexInputItem::Message(notice))] = notices.as_slice() else {
        return Err(format!("expected one notice, got {notices:?}"));
    };
    assert!(notice.content.len() < 1024 && notice.content.contains('\u{2026}'));
    Ok(())
}

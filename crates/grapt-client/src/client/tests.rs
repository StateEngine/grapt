//! End-to-end witnesses over the contract double.
//!
//! The central one is `reconciling_converges_to_durable_truth`: it builds the
//! same session two ways — once by living through the stream with faults
//! injected, once by folding durable truth alone — and requires the two to be
//! equal. Everything else in the module is a specific shape of that claim.

use super::*;
use crate::double::{ContractDouble, Fault};
use crate::projection::{ExecutionState, Outcome, SliceKind, Usage};
use crate::transport::TransportError;
fn client() -> Client<ContractDouble> {
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    // Every witness below reaches a session, and a session cannot be created
    // without a selection. Making it here rather than in each witness keeps
    // those witnesses about what they are named for.
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a project, an agent and an enabled model: {error}");
    }
    client
}

/// Fold durable truth for one session with no live frames at all.
///
/// The reference the convergence invariant compares against. It re-lists
/// through the REAL `SessionMessagesResponseDto` and folds into a view that
/// has never seen a live frame, so nothing live can leak into it.
/// Create a session on a double, refusing to continue if the bound refused.
///
/// `create_session` returns `Option` now, because the producer's session store
/// is bounded. A witness that silently ignored a refusal here would be
/// measuring an empty double.
/// Pump until the stream's positioned connect has proved it, so the next
/// reconcile reads the snapshot.
pub(crate) fn prove<C: Connection>(client: &mut Client<C>) {
    for _ in 0..8 {
        if client.positions.proven {
            return;
        }
        if let Err(error) = client.pump() {
            panic!("pump: {error}");
        }
    }
    panic!("the stream never opened with its positioned connect");
}

fn seed(double: &mut ContractDouble, title: &str) -> crate::wire::SessionWireId {
    double
        .create_session(title)
        .unwrap_or_else(|| panic!("witness session creation must not hit the bound"))
}

/// Raise an ask, refusing to continue if the ask bound refused.
fn seed_ask(
    double: &mut ContractDouble,
    session: &crate::wire::SessionWireId,
    title: &str,
) -> crate::wire::PermissionWireId {
    let raw = double
        .ask_permission(session, title)
        .unwrap_or_else(|| panic!("witness permission ask must not hit the bound"));
    crate::wire::permission_id(&raw)
        .unwrap_or_else(|error| panic!("witness permission id must parse: {error}"))
}

fn durable_only(source: &mut ContractDouble, session: &crate::wire::SessionWireId) -> SessionView {
    // The SAME order the client asks for. The product answers `desc` by
    // default, so a reference built from an unscoped read would fold durable
    // truth backwards and the convergence claim would compare two different
    // orderings — passing only while both were wrong in the same way.
    let Ok(request) = WireRequest::new(Operation::MessageList, Some(session), None) else {
        panic!("the re-list must address");
    };
    let request = request.with_query("order", "asc");
    let Ok(response) = source.call(&request) else {
        panic!("the re-list must answer");
    };
    let page = match crate::wire::decode_messages(&response.body) {
        Ok(page) => page,
        Err(error) => panic!("the re-list must decode as the real DTO: {error}"),
    };
    let mut view = SessionView::new(session);
    for message in &page.messages {
        match view.apply_durable(message, Bounds::PRODUCTION) {
            Ok(_) => {}
            Err(error) => panic!("durable truth must fold cleanly: {error}"),
        }
    }
    view
}

#[test]
fn a_created_session_is_selected_and_listed() {
    let mut client = client();
    let Ok(id) = client.create_session() else {
        panic!("session creation must succeed");
    };
    assert_eq!(client.selected(), Some(&id));
    assert!(client.sessions().iter().any(|s| s.id == id));
    prove(&mut client);
    assert!(
        client.stream_state().is_live(),
        "the proved stream's snapshot is what earns liveness"
    );
}

#[test]
fn commands_without_a_selection_are_refused_rather_than_misrouted() {
    let mut client = client();
    assert_eq!(client.submit_prompt("hi"), Err(ClientError::NoSelection));
    assert_eq!(client.interrupt(), Err(ClientError::NoSelection));
}

#[test]
fn a_fresh_client_is_not_live_until_it_selects() {
    let client = client();
    assert!(
        !client.stream_state().is_live(),
        "a client that never re-listed cannot claim a complete prefix"
    );
    assert!(client.view().is_none());
}

#[test]
fn diagnostics_report_a_healthy_run_as_healthy() {
    // The positive control for every fault witness. A client that reported a
    // gap on everything would satisfy each of them individually.
    let mut client = client();
    let Ok(_) = client.create_session() else {
        panic!("create");
    };
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt");
    };
    let Ok(_) = client.pump() else { panic!("pump") };
    let diagnostics = client.diagnostics();
    assert_eq!(diagnostics.gaps_observed, 0, "no gap on a healthy stream");
    assert_eq!(diagnostics.stream_closes, 0, "no close on a healthy stream");
    assert!(diagnostics.frames_applied > 0, "frames did arrive");
    assert!(client.stream_state().is_live());
}

// ---------------------------------------------------------------------------
// Review blocker 2 - loss intolerance
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Review blocker 4 - bounds at the ownership point
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Form settlement, through the canonical routes
// ---------------------------------------------------------------------------

/// The answer the witnesses below settle with.
fn an_answer() -> std::collections::BTreeMap<String, crate::wire::FormValue> {
    let mut answer = std::collections::BTreeMap::new();
    answer.insert(
        "branch".to_owned(),
        crate::wire::FormValue::Text("main".to_owned()),
    );
    answer.insert("dry_run".to_owned(), crate::wire::FormValue::Flag(true));
    answer
}

/// Raise one form on a fresh selected session, returning both identities.
fn a_raised_form() -> (
    Client<crate::double::ContractDouble>,
    crate::wire::SessionWireId,
    crate::wire::FormWireId,
) {
    let mut client = client();
    let Ok(session) = client.create_session() else {
        panic!("create")
    };
    let Some(form) = client.peer_mut().raise_form(&session, "which branch?") else {
        panic!("raise")
    };
    let Ok(()) = client.refresh_forms() else {
        panic!("forms")
    };
    (client, session, form)
}

// ---------------------------------------------------------------------------
// Paging, as the real product serves it
// ---------------------------------------------------------------------------

/// Build a rebuilt view from durable assistant records, through the REAL codec.
///
/// The records are encoded and decoded exactly as a re-list would be, so these
/// witnesses exercise the contract path rather than hand-built projection
/// state. A shape that could not survive the encoder cannot pass here.
fn rebuilt_from(
    records: Vec<(
        Option<crate::wire::DurableFinish>,
        Option<crate::wire::DurableError>,
    )>,
) -> SessionView {
    let mut messages = Vec::new();
    for (index, (finish, error)) in records.into_iter().enumerate() {
        let id = format!("msg_00000000{:04}", index + 1);
        let Ok(message) = crate::wire::assistant_message(
            &id,
            &[crate::wire::MessageItem::Text("answer".to_owned())],
            finish,
            None,
            error,
        ) else {
            panic!("assistant message")
        };
        messages.push(message);
    }
    let Ok(bytes) = crate::wire::encode_messages(messages, None) else {
        panic!("encode")
    };
    let Ok(page) = crate::wire::decode_messages(&bytes) else {
        panic!("decode")
    };
    let Ok(id) = crate::wire::session_id("ses_000000000001") else {
        panic!("id")
    };
    let mut view = SessionView::new(&id);
    for message in &page.messages {
        let Ok(_) = view.apply_durable(message, Bounds::PRODUCTION) else {
            panic!("apply")
        };
    }
    view
}

fn durable_error(kind: &str, message: &str) -> Option<crate::wire::DurableError> {
    Some(crate::wire::DurableError {
        kind: kind.to_owned(),
        message: message.to_owned(),
    })
}

/// Rebuild a view from tool rows, through the REAL codec.
fn rebuilt_tools(rows: Vec<(&str, &str, &str, crate::wire::ToolStateDouble)>) -> SessionView {
    let mut messages = Vec::new();
    for (message_id, name, call_id, state) in rows {
        let Ok(message) = crate::wire::assistant_tool_message_in(message_id, name, call_id, state)
        else {
            panic!("tool message")
        };
        messages.push(message);
    }
    let Ok(bytes) = crate::wire::encode_messages(messages, None) else {
        panic!("encode")
    };
    let Ok(page) = crate::wire::decode_messages(&bytes) else {
        panic!("decode")
    };
    let Ok(id) = crate::wire::session_id("ses_000000000001") else {
        panic!("id")
    };
    let mut view = SessionView::new(&id);
    for message in &page.messages {
        let Ok(_) = view.apply_durable(message, Bounds::PRODUCTION) else {
            panic!("apply")
        };
    }
    view
}

fn tool_slices(view: &SessionView) -> Vec<&crate::Slice> {
    view.slices
        .iter()
        .filter(|slice| slice.kind == crate::SliceKind::Tool)
        .collect()
}

mod bounds;
mod effort;
mod forms;
mod interruption;
mod live_output;
mod newest_first;
mod paging;
mod refusal;
mod streaming;
mod submission;
mod superseded;
mod tools;

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

#[test]
fn refresh_mcp_reads_all_six_statuses_for_the_selected_project() {
    let mut client = client();
    client.refresh_mcp().unwrap_or_else(|error| {
        panic!("the double must serve the list for the selected project: {error}")
    });
    let rows = client.mcp_servers();
    assert_eq!(
        rows.len(),
        6,
        "one row per member of the frozen union: {rows:?}"
    );
    let words: Vec<&str> = rows.iter().map(|row| row.status.word()).collect();
    assert_eq!(
        words,
        [
            "Connected",
            "Pending",
            "Disabled",
            "Failed",
            "Needs auth",
            "Needs client ID"
        ],
        "the pane must read as the frozen frontend reads"
    );
    assert!(
        client.mcp_refusal().is_none(),
        "a served list is not a refusal: {:?}",
        client.mcp_refusal()
    );
}

#[test]
fn a_refused_mcp_config_is_pane_state_not_an_error() {
    let message = "invalid mcp config: /work/project-a/.grapt/mcp.json";
    let double = ContractDouble::new().refuse_mcp_list(message.to_owned());
    let mut client = Client::new(double, Bounds::PRODUCTION);
    client
        .select_first_offer()
        .unwrap_or_else(|error| panic!("the double must offer a selection: {error}"));
    client
        .refresh_mcp()
        .unwrap_or_else(|error| panic!("a refusal is pane state, not a failure: {error}"));
    assert!(
        client.mcp_servers().is_empty(),
        "a refusal replaces the list, it does not sit beside it: {:?}",
        client.mcp_servers()
    );
    assert_eq!(
        client.mcp_refusal(),
        Some(message),
        "the pane must be able to show the faulting file"
    );
}

#[test]
fn a_bare_400_from_the_mcp_list_stays_a_status() {
    let mut double = ContractDouble::new();
    double.set_fault(Fault::McpUnavailable);
    let mut client = Client::new(double, Bounds::PRODUCTION);
    client
        .select_first_offer()
        .unwrap_or_else(|error| panic!("the double must offer a selection: {error}"));
    let Err(error) = client.refresh_mcp() else {
        panic!("a bare 400 is not pane state")
    };
    assert!(
        matches!(error, ClientError::Status { status: 400, .. }),
        "an unreadable 400 must stay the status it is: {error:?}"
    );
}

#[test]
fn a_mcp_read_without_a_selected_project_is_refused() {
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    assert!(
        matches!(client.refresh_mcp(), Err(ClientError::NoProject)),
        "the list is scoped by the selected project"
    );
}

#[test]
fn a_failed_mcp_read_in_a_new_project_keeps_no_previous_project_truth() {
    // Project A's config is readable; project B's is not. The defect the
    // witness closes: a switch that left A's rows (or A's refusal) on screen
    // under B's name — another project's connection truth as current truth.
    let double = ContractDouble::new().mcp_unavailable_for("/work/project-b");
    let mut client = Client::new(double, Bounds::PRODUCTION);
    client
        .select_first_offer()
        .unwrap_or_else(|error| panic!("the double must offer a selection: {error}"));
    client
        .refresh_mcp()
        .unwrap_or_else(|error| panic!("project A's list must read: {error}"));
    assert_eq!(
        client.mcp_servers().len(),
        6,
        "A's read served its list: {:?}",
        client.mcp_servers()
    );
    let other = client
        .projects()
        .get(1)
        .cloned()
        .unwrap_or_else(|| panic!("the double must offer a second project"));
    client
        .select_project(&other)
        .unwrap_or_else(|error| panic!("the switch must read the new catalog: {error}"));
    let Err(error) = client.refresh_mcp() else {
        panic!("project B's unreadable config must fail its read")
    };
    assert!(
        matches!(error, ClientError::Status { status: 400, .. }),
        "B's unreadable config is the bare 400 it is: {error:?}"
    );
    assert!(
        client.mcp_servers().is_empty(),
        "A's rows are not B's truth: {:?}",
        client.mcp_servers()
    );
    assert!(
        client.mcp_refusal().is_none(),
        "A's refusal is not B's truth: {:?}",
        client.mcp_refusal()
    );
}

#[test]
fn a_failed_mcp_reread_clears_the_rows_it_could_not_refresh() {
    // Same project, second read fails: the pane may not keep rows the failed
    // read never refreshed, and a failed read is not a refusal.
    let double = ContractDouble::new().mcp_fail_after_reads(1);
    let mut client = Client::new(double, Bounds::PRODUCTION);
    client
        .select_first_offer()
        .unwrap_or_else(|error| panic!("the double must offer a selection: {error}"));
    client
        .refresh_mcp()
        .unwrap_or_else(|error| panic!("the first read must serve the list: {error}"));
    assert_eq!(
        client.mcp_servers().len(),
        6,
        "the first read served rows: {:?}",
        client.mcp_servers()
    );
    let Err(error) = client.refresh_mcp() else {
        panic!("the second read must fail")
    };
    assert!(
        matches!(error, ClientError::Status { status: 400, .. }),
        "the failed reread is the bare 400 it is: {error:?}"
    );
    assert!(
        client.mcp_servers().is_empty(),
        "a read that failed must not leave rows it could not refresh: {:?}",
        client.mcp_servers()
    );
    assert!(
        client.mcp_refusal().is_none(),
        "a failed read is not a refusal: {:?}",
        client.mcp_refusal()
    );
}

#[test]
fn an_oversized_400_body_keeps_its_status_rather_than_retaining_the_refusal() {
    // A valid 6000-byte refusal against a 4096-byte client bound: the decode
    // must stay inside the bound, and the status precedence must hold — an
    // oversized error is reported as the status, never as retained content.
    let double = ContractDouble::new().refuse_mcp_list("x".repeat(6000));
    let mut client = Client::new(
        double,
        Bounds {
            max_response_bytes: 4096,
            ..Bounds::PRODUCTION
        },
    );
    client
        .select_first_offer()
        .unwrap_or_else(|error| panic!("the double must offer a selection: {error}"));
    let Err(error) = client.refresh_mcp() else {
        panic!("a 6000-byte refusal must not pass the 4096-byte bound")
    };
    assert!(
        matches!(error, ClientError::Status { status: 400, .. }),
        "an oversized error keeps the status precedence: {error:?}"
    );
    assert!(
        client.mcp_refusal().is_none(),
        "oversized content must not become retained pane state: {:?}",
        client.mcp_refusal()
    );
}

#[test]
fn a_400_within_the_client_bound_still_decodes_its_refusal() {
    // The same 400 inside an explicit bound decodes to its refusal: the
    // bound guards the decode, it does not replace it. The bound is raised
    // to the size the double's own catalog reads need; the refusal body
    // stays far inside it.
    let message = "invalid mcp config: /work/project-a/.grapt/mcp.json";
    let double = ContractDouble::new().refuse_mcp_list(message.to_owned());
    let mut client = Client::new(
        double,
        Bounds {
            max_response_bytes: 8 * 1024,
            ..Bounds::PRODUCTION
        },
    );
    client
        .select_first_offer()
        .unwrap_or_else(|error| panic!("the double must offer a selection: {error}"));
    client
        .refresh_mcp()
        .unwrap_or_else(|error| panic!("a 400 inside the bound is pane state: {error}"));
    assert_eq!(
        client.mcp_refusal(),
        Some(message),
        "the in-bound refusal must name the file: {:?}",
        client.mcp_refusal()
    );
}
mod recovery;

mod presets;
mod rewind;
#[path = "tests/undo.rs"]
mod undo;

#[test]
fn mail_attention_survives_relist_but_stream_loss_invalidates_it(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_api::u3a::{MailAttentionInfo, MailPendingCount};
    let mut client = client();
    let session = client.create_session()?;
    prove(&mut client);
    let attention = MailAttentionInfo::Ready {
        pending: MailPendingCount::new(7).ok_or("pending")?,
        incomplete: true,
        decode_failures: Default::default(),
    };
    client.views.get_mut(&session).ok_or("view")?.mail_attention = Some(attention);
    client.reconcile(&session)?;
    assert_eq!(client.view().ok_or("view")?.mail_attention, Some(attention));
    client.peer_mut().set_fault(Fault::CloseStream);
    let _reconnect = client.pump();
    assert_eq!(client.view().ok_or("view")?.mail_attention, None);
    Ok(())
}

#[test]
fn an_install_keeps_only_this_connections_continuity_level(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_api::u3a::{ContinuityAttentionInfo, ContinuityDegradedReason};
    let mut client = client();
    let session = client.create_session()?;
    prove(&mut client);
    let degraded = ContinuityAttentionInfo::Degraded {
        reason: ContinuityDegradedReason::Capability,
    };
    client
        .views
        .get_mut(&session)
        .ok_or("view")?
        .continuity_attention = Some(degraded);
    client.reconcile(&session)?;
    assert_eq!(
        client.view().ok_or("view")?.continuity_attention,
        Some(degraded)
    );
    // Losing the stream makes the level unknown, never a stale degraded.
    client.peer_mut().set_fault(Fault::CloseStream);
    let _reconnect = client.pump();
    assert_eq!(client.view().ok_or("view")?.continuity_attention, None);
    // The reconnect's hydration reports healthy; the re-read that follows
    // keeps that, and cannot bring back the level from before the loss.
    client
        .views
        .get_mut(&session)
        .ok_or("view")?
        .continuity_attention = Some(ContinuityAttentionInfo::Ok);
    client.reconcile(&session)?;
    assert_eq!(
        client.view().ok_or("view")?.continuity_attention,
        Some(ContinuityAttentionInfo::Ok)
    );
    Ok(())
}

mod session_management;

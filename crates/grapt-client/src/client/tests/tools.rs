//! tools witnesses, split from the carrier by subject.
//!
//! Moved unchanged: the ratchet bounds a FILE, and a witness that was edited
//! on its way out of one would be a different witness.

use super::*;

mod timeout;

#[derive(Debug)]
struct ProgressPeer {
    inner: ContractDouble,
    page: Vec<u8>,
    frames: std::collections::VecDeque<FramePoll>,
    reads: usize,
    subscriptions: usize,
    closed: bool,
}
impl Transport for ProgressPeer {
    fn call(
        &mut self,
        request: &WireRequest,
    ) -> Result<crate::transport::WireResponse, TransportError> {
        if request.operation() == Operation::SessionSnapshot {
            self.reads += 1;
            let unavailable = |error: &dyn std::fmt::Display| TransportError::Unavailable {
                reason: error.to_string(),
            };
            let page = newest_first(&self.page).map_err(|error| unavailable(&error))?;
            let body = crate::double::page_snapshot(request, &page, 0)
                .map_err(|error| unavailable(&error))?;
            return Ok(crate::transport::WireResponse { status: 200, body });
        }
        if request.operation() == Operation::MessageList {
            assert!(
                request
                    .query()
                    .iter()
                    .any(|(key, value)| key == "diagnostics" && value == "cache"),
                "TUI history reads must request the live cache view"
            );
            self.reads += 1;
            // The fixed page is written oldest-first; the product serves it
            // newest-first unless asked for `asc`, and so does this peer.
            let ascending = request
                .query()
                .iter()
                .any(|(key, value)| key == "order" && value == "asc");
            let mut body = self.page.clone();
            if !ascending {
                body = newest_first(&body).map_err(|error| TransportError::Unavailable {
                    reason: error.to_string(),
                })?;
            }
            return Ok(crate::transport::WireResponse { status: 200, body });
        }
        if request.operation() == Operation::SessionPendingList {
            return Ok(WireResponse {
                status: 200,
                body: br#"{"data":[]}"#.to_vec(),
            });
        }
        self.inner.call(request)
    }
}
/// The same page with its rows in the opposite order, through the real DTO
/// codec rather than a JSON round trip that would lose its numeric shapes.
fn newest_first(body: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let page: grapt_api::u3a::SessionMessagesResponseDto =
        grapt_api::u3a::decode_page(body, crate::wire::limits()?)?;
    let mut rows = page.data().get().as_slice().to_vec();
    rows.reverse();
    Ok(crate::wire::encode_messages(rows, None)?)
}
impl Connection for ProgressPeer {
    fn subscribed(&self) -> bool {
        !self.closed
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        if let Some(connect) = self.inner.connect() {
            return Ok(connect);
        }
        let frame = self.frames.pop_front().unwrap_or(FramePoll::Idle);
        if matches!(frame, FramePoll::Closed) {
            self.closed = true;
            return Ok(frame);
        }
        Ok(self.inner.serve(frame))
    }
    fn resubscribe(&mut self) {
        self.subscriptions += 1;
        self.closed = false;
        self.inner.resubscribe();
    }
}

fn progress_page(completed: bool) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let tool = crate::wire::DurableTool {
        duration_ms: None,
        id: crate::wire::tool_call_id("call_000000000001")?,
        name: "execute".to_owned(),
        state: if completed {
            crate::wire::DurableToolState::Completed {
                input: "{}".into(),
                output: "14".into(),
            }
        } else {
            crate::wire::DurableToolState::Running { input: "{}".into() }
        },
        progress: Vec::new(),
    };
    let message = crate::wire::assistant_message(
        "msg_000000000001",
        &[crate::wire::MessageItem::Tool(tool)],
        completed.then_some(crate::wire::DurableFinish::Stop),
        None,
        None,
    )?;
    let bytes = crate::wire::encode_messages(vec![message], None)?;
    let metadata = serde_json::json!({"toolCalls":[{
        "tool":"fixture.add","status":if completed { "completed" } else { "running" }
    }]});
    // Preserve the DTO's discriminant-first and numeric wire representation.
    let page = String::from_utf8(bytes)?;
    let needle = if completed {
        "\"status\":\"completed\""
    } else {
        "\"metadata\":{}"
    };
    assert_eq!(page.matches(needle).count(), 1);
    let replacement = if completed {
        format!("{needle},\"metadata\":{metadata}")
    } else {
        format!("\"metadata\":{metadata}")
    };
    Ok(page.replacen(needle, &replacement, 1).into_bytes())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn codemode_progress_coalesces_preserves_live_text_and_reconnects_to_history(
) -> Result<(), Box<dyn std::error::Error>> {
    let session = crate::wire::session_id("ses_000000000001")?;
    let peer = ProgressPeer {
        inner: ContractDouble::new(),
        page: progress_page(false)?,
        frames: std::collections::VecDeque::new(),
        reads: 0,
        subscriptions: 0,
        closed: false,
    };
    let mut client = Client::new(peer, Bounds::PRODUCTION);
    client.selected = Some(session.clone());
    prove(&mut client);
    client.reconcile(&session)?;
    let frame = |kind: &str, data: serde_json::Value| {
        let mut value = serde_json::json!({
            "id":"evt_progress_test","type":kind,"created":1,"data":data
        });
        if crate::contract::EventMember::parse(kind).is_some_and(|member| member.requires_durable())
        {
            value["durable"] =
                serde_json::json!({"aggregateID":session.as_inner().as_str(),"seq":1,"version":1});
        }
        value.to_string()
    };
    for (kind, data) in [
        (
            "session.execution.started",
            serde_json::json!({"sessionID":session.as_inner().as_str(),"executionID":"exe_1"}),
        ),
        (
            "session.text.started",
            serde_json::json!({"assistantMessageID":"msg_fixture_answer","ordinal":0,"sessionID":session.as_inner().as_str()}),
        ),
        (
            "session.text.delta",
            serde_json::json!({"assistantMessageID":"msg_fixture_answer","ordinal":0,"sessionID":session.as_inner().as_str(),"delta":"UNFINISHED","from":0}),
        ),
    ] {
        let envelope = EventEnvelope::decode(&frame(kind, data))?;
        client
            .views
            .get_mut(&session)
            .ok_or("view absent")?
            .apply(&envelope, Bounds::PRODUCTION)?;
    }
    let data = serde_json::json!({"sessionID":session.as_inner().as_str(),"assistantMessageID":"msg_000000000001",
        "callID":"call_000000000001","metadata":{"toolCalls":[{"tool":"fixture.add","status":"running"}]}});
    let hint = frame("session.tool.progress", data.clone());
    client.peer_mut().reads = 0;
    client.peer_mut().frames.extend([
        FramePoll::Frame(hint.clone()),
        FramePoll::Frame(hint.clone()),
    ]);
    assert!(client.pump()?);
    assert_eq!(client.peer_mut().reads, 1);
    assert_eq!(client.peer_mut().subscriptions, 0);
    assert!(client
        .view()
        .ok_or("view")?
        .slices
        .iter()
        .any(|slice| !slice.closed && slice.text.contains("UNFINISHED")));
    let mut foreign = data.clone();
    foreign["sessionID"] = serde_json::json!("ses_000000000002");
    let mut malformed = data;
    malformed["metadata"]["toolCalls"][0]["private"] = serde_json::json!("hidden");
    client
        .peer_mut()
        .frames
        .push_back(FramePoll::Frame(frame("session.tool.progress", foreign)));
    assert!(!client.pump()?);
    assert_eq!(
        client.view().and_then(SessionView::streamed_text_bytes),
        Some(10)
    );
    client
        .peer_mut()
        .frames
        .push_back(FramePoll::Frame(frame("session.tool.progress", malformed)));
    // Refusing a malformed hint leaves the tool rows untouched, but loses
    // continuity for the footer's live-only output estimate.
    assert!(client.pump()?);
    assert_eq!(
        client.view().and_then(SessionView::streamed_text_bytes),
        None
    );
    assert_eq!(client.peer_mut().reads, 1);
    client.peer_mut().page = progress_page(true)?;
    client
        .peer_mut()
        .frames
        .push_back(FramePoll::Frame(hint.clone()));
    assert!(client.pump()?);
    let settled = client.view().ok_or("view")?.clone();
    client.peer_mut().page = progress_page(false)?;
    client.peer_mut().frames.push_back(FramePoll::Frame(hint));
    assert!(client.pump()?);
    assert_eq!(client.view(), Some(&settled));
    client.peer_mut().page = progress_page(true)?;
    client.peer_mut().frames.push_back(FramePoll::Closed);
    assert!(client.pump()?);
    assert_eq!(client.peer_mut().subscriptions, 1);
    prove(&mut client);
    let page = crate::wire::decode_messages(&progress_page(true)?)?;
    let mut expected = history::tail_window(&session, &page, Bounds::PRODUCTION)?;
    expected.cache.history_complete(true);
    // An install also holds the terminal its newest record settled.
    expected.held_terminal = history::tail_terminal(&page);
    assert_eq!(client.view(), Some(&expected));
    Ok(())
}

#[test]
fn codemode_settled_tool_cannot_be_reopened_by_stale_progress(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_api::u3a::{ToolProgressEntry, ToolProgressStatus};
    let session = crate::wire::session_id("ses_000000000001")?;
    let mut view = SessionView::new(&session);
    let mut tool = crate::wire::DurableTool {
        duration_ms: None,
        id: crate::wire::tool_call_id("call_000000000001")?,
        name: "execute".to_owned(),
        state: crate::wire::DurableToolState::Running {
            input: "{}".to_owned(),
        },
        progress: vec![ToolProgressEntry::new(
            "fixture.add",
            ToolProgressStatus::Running,
        )?],
    };
    view.push_tool(&tool, Bounds::PRODUCTION);
    assert!(tool_slices(&view)[0].text.contains("fixture.add running"));
    let stale = tool.clone();
    tool.state = crate::wire::DurableToolState::Completed {
        input: "{}".to_owned(),
        output: "14".to_owned(),
    };
    tool.progress = vec![ToolProgressEntry::new(
        "fixture.add",
        ToolProgressStatus::Completed,
    )?];
    view.push_tool(&tool, Bounds::PRODUCTION);
    let final_view = view.clone();
    view.push_tool(&stale, Bounds::PRODUCTION);
    view.push_tool(&stale, Bounds::PRODUCTION);
    assert_eq!(view, final_view);
    assert!(tool_slices(&view)[0].text.contains("fixture.add completed"));
    Ok(())
}

#[test]
fn a_completed_tool_shows_what_ran_and_what_it_produced() {
    // A client that gates real work behind an approval and then cannot show
    // what it approved is asking the user to authorise something invisible.
    // Identity, ARGUMENTS and output all have to survive the rebuild.
    let view = rebuilt_tools(vec![(
        "msg_000000000001",
        "read",
        "call_000000000001",
        crate::wire::ToolStateDouble::Completed {
            input: "{\"path\":\"j03.txt\"}".to_owned(),
            output: "ALPHAMARKER".to_owned(),
        },
    )]);
    let slices = tool_slices(&view);
    assert_eq!(slices.len(), 1, "one call, one row");
    let text = &slices[0].text;
    assert!(text.starts_with("Read j03.txt"), "the tool's name: {text}");
    assert!(slices[0].closed, "its settled outcome: {text}");
    assert_eq!(
        slices[0]
            .tool_id
            .as_ref()
            .map(crate::wire::Spelled::spelled),
        Some("call_000000000001"),
        "its identity survives presentation"
    );
    assert!(
        text.contains("Raw output preview:"),
        "unformatted output is labelled: {text}"
    );
    assert!(text.contains("j03.txt"), "the arguments approved: {text}");
    assert!(text.contains("ALPHAMARKER"), "and what it produced: {text}");
}

#[test]
fn a_failed_tool_keeps_the_products_own_account_of_the_failure() {
    // The actionable-error law. An error dropped here is the sentence the user
    // needed most, and `unsupported` counting is not a substitute for it.
    let view = rebuilt_tools(vec![(
        "msg_000000000001",
        "shell",
        "call_000000000002",
        crate::wire::ToolStateDouble::Error {
            input: "{\"command\":\"false\"}".to_owned(),
            kind: "tool.failed".to_owned(),
            message: "the command exited non-zero".to_owned(),
        },
    )]);
    let slices = tool_slices(&view);
    assert_eq!(slices.len(), 1);
    let text = &slices[0].text;
    assert!(text.contains("error"), "the outcome: {text}");
    assert!(text.contains("tool.failed"), "the error's type: {text}");
    assert!(
        text.contains("the command exited non-zero"),
        "and the product's own words: {text}"
    );
}

#[test]
fn a_running_tool_is_not_dressed_as_a_finished_one() {
    let view = rebuilt_tools(vec![(
        "msg_000000000001",
        "shell",
        "call_000000000003",
        crate::wire::ToolStateDouble::Running,
    )]);
    let slices = tool_slices(&view);
    assert_eq!(slices.len(), 1);
    assert!(
        slices[0].text.contains("running"),
        "a call still in flight says so: {}",
        slices[0].text
    );
    assert!(
        !slices[0].text.contains("completed"),
        "and does not claim a terminal it has not reached: {}",
        slices[0].text
    );
}

#[test]
fn re_listing_the_same_call_updates_one_row_rather_than_appending_a_second() {
    // RECONNECT DEDUPLICATION, and the reason it is by identity rather than by
    // position: reconciliation re-lists pages this client may already hold, so
    // the same invocation arrives more than once. Appending would show one
    // `read` twice and invent work that never happened.
    //
    // The two rows here are the SAME call seen at two stages, which is exactly
    // what a reconnect mid-call produces.
    let view = rebuilt_tools(vec![
        (
            "msg_000000000001",
            "read",
            "call_000000000001",
            crate::wire::ToolStateDouble::Running,
        ),
        (
            "msg_000000000002",
            "read",
            "call_000000000001",
            crate::wire::ToolStateDouble::Completed {
                input: "{\"path\":\"j03.txt\"}".to_owned(),
                output: "ALPHAMARKER".to_owned(),
            },
        ),
    ]);
    let slices = tool_slices(&view);
    assert_eq!(
        slices.len(),
        1,
        "one invocation is one row however many times it is re-listed"
    );
    let text = &slices[0].text;
    assert!(
        slices[0].closed && text.contains("ALPHAMARKER"),
        "and the row carries the LATER state, not the stale one: {text}"
    );
    assert!(
        !text.contains("running"),
        "the superseded stage is gone rather than kept alongside: {text}"
    );
}

#[test]
fn two_different_calls_are_two_rows() {
    // The other side of deduplication: identity distinguishes, so distinct
    // invocations must not collapse into one.
    let view = rebuilt_tools(vec![
        (
            "msg_000000000001",
            "read",
            "call_000000000001",
            crate::wire::ToolStateDouble::Running,
        ),
        (
            "msg_000000000002",
            "shell",
            "call_000000000002",
            crate::wire::ToolStateDouble::Running,
        ),
    ]);
    assert_eq!(tool_slices(&view).len(), 2, "two calls, two rows");
}

#[test]
fn a_tool_between_two_answers_stays_between_them() {
    // THE MUTANT THIS EXISTS TO KILL, and it shipped once.
    //
    // Decoding tools into a list of their own and rendering them after the text
    // turned `[text, tool, text]` into `[text, text, tool]`, so a reader saw an
    // answer referring to work that appeared below it. Every earlier tool
    // witness used a message containing NOTHING but a tool, which is exactly
    // the shape in which that reordering is invisible.
    let tool = crate::wire::DurableTool {
        duration_ms: None,
        progress: Vec::new(),
        id: match crate::wire::tool_call_id("call_000000000009") {
            Ok(id) => id,
            Err(error) => panic!("tool call id: {error}"),
        },
        name: "shell".to_owned(),
        state: crate::wire::DurableToolState::Completed {
            input: "{\"command\":\"echo\"}".to_owned(),
            output: "MIDDLE".to_owned(),
        },
    };
    let Ok(message) = crate::wire::assistant_message(
        "msg_000000000001",
        &[
            crate::wire::MessageItem::Text("BEFORE".to_owned()),
            crate::wire::MessageItem::Tool(tool),
            crate::wire::MessageItem::Text("AFTER".to_owned()),
        ],
        Some(crate::wire::DurableFinish::Stop),
        None,
        None,
    ) else {
        panic!("assistant message")
    };
    let Ok(bytes) = crate::wire::encode_messages(vec![message], None) else {
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
    let order: Vec<(SliceKind, &str)> = view
        .slices
        .iter()
        .map(|slice| (slice.kind, slice.text.as_str()))
        .collect();
    assert_eq!(
        order.len(),
        4,
        "three content items and one footer: {order:?}"
    );
    assert_eq!(order[3].0, SliceKind::AssistantFooter);
    let footer = view.slices[3]
        .footer
        .as_ref()
        .unwrap_or_else(|| panic!("typed footer absent"));
    assert_eq!(footer.message.as_inner().as_str(), "msg_000000000001");
    assert_eq!(order[0].0, SliceKind::Text);
    assert!(order[0].1.contains("BEFORE"), "{order:?}");
    assert_eq!(
        order[1].0,
        SliceKind::Tool,
        "the tool sits where the contract put it: {order:?}"
    );
    assert!(order[1].1.contains("shell"), "{order:?}");
    assert_eq!(order[2].0, SliceKind::Text);
    assert!(order[2].1.contains("AFTER"), "{order:?}");
}

#[test]
fn a_refusal_says_what_to_do_about_it_and_never_invents_a_meaning() {
    // A BARE NUMBER IS NOT ACTIONABLE.
    //
    // `401` and `503` both rendered as "answered <n>". One needs the user to
    // authenticate and the other needs them to wait, and nothing on screen
    // separated them. The load-bearing distinction is whether retrying the
    // same thing can possibly work.
    let authenticate = ClientError::Status {
        operation: "session prompt",
        status: 401,
    }
    .to_string();
    assert!(
        authenticate.contains("401") && authenticate.contains("sign in"),
        "401 keeps its number and says what to do: {authenticate}"
    );

    let wait = ClientError::Status {
        operation: "session prompt",
        status: 503,
    }
    .to_string();
    assert!(
        wait.contains("503") && wait.contains("retrying can work"),
        "503 says retrying is worth it: {wait}"
    );
    assert_ne!(
        authenticate, wait,
        "the two refusals must not read the same"
    );

    // AND A CLIENT DEFECT IS NOT DRESSED AS A TRANSIENT ONE.
    let refused = ClientError::Status {
        operation: "session prompt",
        status: 400,
    }
    .to_string();
    assert!(
        refused.contains("will not help"),
        "400 says retrying unchanged is pointless: {refused}"
    );

    // THE HONEST HALF. The gloss is the frozen contract's documented set, so a
    // status it never documents gets NO meaning invented for it -- the number
    // stands alone rather than borrowing a general HTTP table this service
    // never promised to follow.
    let undocumented = ClientError::Status {
        operation: "session prompt",
        status: 418,
    }
    .to_string();
    assert_eq!(
        undocumented, "session prompt answered 418",
        "an undocumented status is reported, not interpreted: {undocumented}"
    );
}

#[test]
fn an_edit_shows_what_changed_rather_than_its_serialisation() {
    // ALPHA'S ATTENDED FINDING on presentation. A completed edit rendered as
    // one raw JSON row -- the retained client showed the actual change. A
    // reader approving or reviewing an edit wants the before and the after,
    // and those ARE the fields; the braces, quotes and escapes are the
    // serialisation's and only got in the way.
    let view = rebuilt_tools(vec![(
        "msg_000000000001",
        "edit",
        "call_000000000003",
        crate::wire::ToolStateDouble::Completed {
            input: "{\"filePath\":\"j03.txt\",\"oldString\":\"ALPHAMARKER\",\"newString\":\"BETAMARKER\"}"
                .to_owned(),
            output: "1 replacement".to_owned(),
        },
    )]);
    let slices = tool_slices(&view);
    assert_eq!(slices.len(), 1, "one call, one row");
    let text = &slices[0].text;

    // Both halves of the change are present and each is on ITS OWN ROW, so a
    // narrow pane wraps them independently instead of cutting the second one
    // off the end of a single long line.
    let rows: Vec<&str> = text.lines().collect();
    let old_row = rows
        .iter()
        .find(|row| row.contains("ALPHAMARKER"))
        .unwrap_or_else(|| panic!("the replaced text is shown: {text}"));
    let new_row = rows
        .iter()
        .find(|row| row.contains("BETAMARKER"))
        .unwrap_or_else(|| panic!("the replacement is shown: {text}"));
    assert_ne!(old_row, new_row, "on separate rows: {text}");
    assert!(text.contains("j03.txt"), "and the file it touched: {text}");

    // THE SERIALISATION IS GONE, asserted on the exact rows. Checking only
    // that the values appear somewhere would pass on the raw JSON that caused
    // this finding, since the values appear in that too.
    assert!(
        old_row.trim() == "oldString ALPHAMARKER",
        "the row is the field and its value, unquoted: {old_row:?}"
    );
    assert!(
        new_row.trim() == "newString BETAMARKER",
        "and so is the replacement: {new_row:?}"
    );
    assert!(
        !text.contains('{') && !text.contains('"'),
        "no JSON punctuation survives: {text}"
    );
}

#[test]
fn a_tool_with_no_arguments_still_says_so() {
    // THE REACHABLE FALLBACK. The wire types a tool input as `OpenObject`, so
    // a payload that is not an object cannot arrive at all -- asserting on one
    // would be asserting about a shape the contract cannot carry. What CAN
    // arrive is an EMPTY object, and it must render as something rather than
    // silently leaving the row looking like a tool that took no input because
    // the client dropped it.
    let view = rebuilt_tools(vec![(
        "msg_000000000001",
        "shell",
        "call_000000000004",
        crate::wire::ToolStateDouble::Completed {
            input: "{}".to_owned(),
            output: "done".to_owned(),
        },
    )]);
    let slices = tool_slices(&view);
    let text = &slices[0].text;
    assert!(
        text.contains("input {}"),
        "an empty argument object is shown as empty, not omitted: {text}"
    );
    assert!(text.contains("done"), "and the output survives: {text}");
}

#[test]
fn tool_lifecycle_hints_refresh_only_the_selected_session() -> Result<(), Box<dyn std::error::Error>>
{
    use crate::contract::EventMember;
    use crate::wire::Spelled;
    let selected = crate::wire::session_id("ses_000000000001")?;
    let peer = ProgressPeer {
        inner: ContractDouble::new(),
        page: progress_page(true)?,
        frames: std::collections::VecDeque::new(),
        reads: 0,
        subscriptions: 0,
        closed: false,
    };
    let mut client = Client::new(peer, Bounds::PRODUCTION);
    client.selected = Some(selected.clone());
    client.cursor.adopt_baseline(&selected, 0);
    client
        .views
        .insert(selected.clone(), SessionView::new(&selected));
    for member in [
        EventMember::ToolInputStarted,
        EventMember::ToolCalled,
        EventMember::ToolSuccess,
        EventMember::ToolFailed,
    ] {
        let version = if matches!(member, EventMember::ToolSuccess | EventMember::ToolFailed) {
            2
        } else {
            1
        };
        let hint = |session: &str| {
            serde_json::json!({
                "id":"evt_tool_hint", "type":member.as_str(),"created":1,
                "durable":{"aggregateID":session,"seq":1,"version":version},
                "data":{"sessionID":session,"callID":"call_000000000001"}
            })
            .to_string()
        };
        let reads = client.peer_mut().reads;
        client
            .peer_mut()
            .frames
            .push_back(FramePoll::Frame(hint("ses_000000000002")));
        assert!(
            !client.pump()?,
            "foreign {} changed selected view",
            member.as_str()
        );
        assert_eq!(
            client.peer_mut().reads,
            reads,
            "foreign tool hint read selected history"
        );
        let own = hint(selected.spelled());
        client
            .peer_mut()
            .frames
            .extend([FramePoll::Frame(own.clone()), FramePoll::Frame(own)]);
        assert!(client.pump()?);
        assert_eq!(
            client.peer_mut().reads,
            reads + 1,
            "matching hints must coalesce"
        );
        assert_eq!(client.peer_mut().subscriptions, 0);
        assert!(client
            .view()
            .ok_or("selected view")?
            .slices
            .iter()
            .any(|slice| slice.text.contains("14")));
    }
    Ok(())
}

mod cache;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn mcp_progress_is_displayed_without_history_writes_and_clears_on_recovery_or_completion(
) -> Result<(), Box<dyn std::error::Error>> {
    let session = crate::wire::session_id("ses_000000000001")?;
    let peer = ProgressPeer {
        inner: ContractDouble::new(),
        page: progress_page(false)?,
        frames: Default::default(),
        reads: 0,
        subscriptions: 0,
        closed: false,
    };
    let mut client = Client::new(peer, Bounds::PRODUCTION);
    client.selected = Some(session.clone());
    prove(&mut client);
    client.reconcile(&session)?;
    let event = |owner: &str, message: &str| {
        serde_json::json!({
        "id":"evt_mcp_progress", "type":"session.tool.progress", "created":1,
        "data":{"sessionID":owner,"assistantMessageID":"msg_000000000001","callID":"call_000000000001",
            "metadata":{"toolCalls":[{"tool":"fixture_echo","status":"running","message":message}]}}
    }).to_string()
    };
    let result = |client: &Client<ProgressPeer>| {
        client.view().and_then(|view| {
            view.slices.iter().find_map(|slice| {
                slice
                    .tool_summary
                    .as_ref()
                    .map(|summary| summary.result.clone())
            })
        })
    };
    client.peer_mut().reads = 0;
    client
        .peer_mut()
        .frames
        .push_back(FramePoll::Frame(event(session.as_inner().as_str(), "42%")));
    assert!(client.pump()?);
    assert_eq!(result(&client).as_deref(), Some("42%"));
    assert_eq!(
        client.peer_mut().reads,
        0,
        "live display fetched durable progress"
    );
    client
        .peer_mut()
        .frames
        .push_back(FramePoll::Frame(event(session.as_inner().as_str(), "")));
    client.pump()?;
    assert_eq!(result(&client).as_deref(), Some(""));

    let child_event = |source: &str, message: &str| {
        let mut value: serde_json::Value =
            serde_json::from_str(&event(session.as_inner().as_str(), message))
                .unwrap_or_else(|error| panic!("event: {error}"));
        value["data"]["sourceCallID"] = source.into();
        value.to_string()
    };
    for (source, message, expected) in [
        ("call_child_a", "A: 42%", "A: 42%"),
        ("call_child_b", "B: 50%", "B: 50%"),
        ("call_child_a", "", "B: 50%"),
        ("call_child_b", "", ""),
        ("call_child_a", "A: 60%", "A: 60%"),
        ("call_child_b", "B: 70%", "B: 70%"),
        ("call_child_b", "", "A: 60%"),
        ("call_child_a", "", ""),
    ] {
        client
            .peer_mut()
            .frames
            .push_back(FramePoll::Frame(child_event(source, message)));
        client.pump()?;
        assert_eq!(
            result(&client).as_deref(),
            Some(expected),
            "one parallel child cleared another child's progress"
        );
    }
    for (owner, message) in [
        ("ses_other", "FOREIGN"),
        (session.as_inner().as_str(), "bad\u{1b}control"),
    ] {
        client
            .peer_mut()
            .frames
            .push_back(FramePoll::Frame(event(owner, message)));
        client.pump()?;
        assert_ne!(result(&client).as_deref(), Some(message));
    }
    prove(&mut client);
    client.reconcile(&session)?;
    assert_ne!(result(&client).as_deref(), Some("42%"));
    // A first progress hint may beat hydration of its durable call row.
    client
        .views
        .insert(session.clone(), SessionView::new(&session));
    client.peer_mut().frames.push_back(FramePoll::Frame(event(
        session.as_inner().as_str(),
        "Staging",
    )));
    client.pump()?;
    assert_eq!(result(&client).as_deref(), Some("Staging"));
    client.peer_mut().page = progress_page(true)?;
    prove(&mut client);
    client.reconcile(&session)?;
    let settled = client.view().cloned();
    client.peer_mut().reads = 0;
    client
        .peer_mut()
        .frames
        .push_back(FramePoll::Frame(event(session.as_inner().as_str(), "LATE")));
    client.pump()?;
    assert_eq!(client.view(), settled.as_ref());
    assert_eq!(
        client.peer_mut().reads,
        0,
        "late progress refreshed a completed tool"
    );
    Ok(())
}

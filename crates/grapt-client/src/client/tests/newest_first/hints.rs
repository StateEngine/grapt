//! A tool hint refreshes only what is on screen, within bounded reads.

use super::*;

fn hint(client: &mut Client<HeldPeer>, id: &crate::wire::SessionWireId, call: usize) {
    let frame = serde_json::json!({
        "id": format!("evt_hint_{call}"), "type": "session.tool.progress", "created": 1,
        "data": {
            "sessionID": id.as_inner().as_str(),
            "assistantMessageID": "msg_000000000001",
            "callID": format!("call_{call:012}"),
            "metadata": { "toolCalls": [ { "tool": "fixture.add", "status": "running" } ] }
        }
    });
    client
        .peer_mut()
        .frames
        .push_back(FramePoll::Frame(frame.to_string()));
    let Ok(_) = client.pump() else {
        panic!("pump the hint")
    };
}

fn tool_texts(client: &Client<HeldPeer>) -> Vec<String> {
    client
        .view()
        .map(|view| {
            view.slices
                .iter()
                .filter(|slice| slice.tool_id.is_some())
                .map(|slice| slice.text.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The most newest-first pages one refresh may read: enough rows to cover
/// the retained window when every row folds to one slice, plus the page that
/// crosses it.
fn window_pages(rows_per_page: usize) -> usize {
    Bounds::PRODUCTION
        .max_retained_slices
        .div_ceil(rows_per_page)
        + 1
}

#[test]
fn a_hint_for_a_call_older_than_the_retained_window_reads_a_bounded_window() {
    let mut rows = vec![Synthetic::User, Synthetic::Tool];
    rows.extend(turns(2_000));
    let (mut client, id) = seeded(Bounds::PRODUCTION, rows);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    let shown = texts(&client);
    let before = client.peer_mut().inner.message_list_calls();
    hint(&mut client, &id, 1);
    let reads = client.peer_mut().inner.message_list_calls() - before;
    assert!(
        (1..=window_pages(50)).contains(&reads),
        "the walk stops at the retained window: {reads} pages"
    );
    assert_eq!(texts(&client), shown, "a call off screen is not appended");
}

#[test]
fn a_hint_for_a_call_absent_from_history_reads_a_bounded_window() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(2_000));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    let shown = texts(&client);
    let before = client.peer_mut().inner.message_list_calls();
    hint(&mut client, &id, 7_777);
    let reads = client.peer_mut().inner.message_list_calls() - before;
    assert!((1..=window_pages(50)).contains(&reads), "{reads} pages");
    assert_eq!(texts(&client), shown);
}

#[test]
fn a_hint_follows_byte_short_pages_to_a_call_on_screen() {
    let mut rows = turns(100);
    rows[60] = Synthetic::Tool;
    let (mut client, id) = seeded(Bounds::PRODUCTION, rows);
    client.peer_mut().inner.cut_pages_at(7);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    assert!(tool_texts(&client)
        .iter()
        .all(|text| text.contains("running")));
    client.peer_mut().inner.finish_tools();
    let before = client.peer_mut().inner.message_list_calls();
    hint(&mut client, &id, 60);
    let reads = client.peer_mut().inner.message_list_calls() - before;
    assert_eq!(
        reads,
        40usize.div_ceil(7),
        "newest-first to the call, no further"
    );
    let tools = tool_texts(&client);
    assert_eq!(tools.len(), 1, "updated in place, not appended: {tools:?}");
    assert!(tools[0].contains("completed"), "{tools:?}");
}

#[test]
fn a_hint_past_large_unrelated_replies_keeps_only_the_hinted_call() {
    let mut rows = vec![Synthetic::User, Synthetic::Tool];
    // Two slices per completed reply (content + footer), keeping the hinted
    // call inside the same production retention bound.
    let unrelated = 200usize;
    rows.extend(std::iter::repeat_n(Synthetic::Large, unrelated));
    let (mut client, id) = seeded(Bounds::PRODUCTION, rows);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    client.peer_mut().inner.finish_tools();
    let shown = texts(&client).len();
    let before = client.peer_mut().inner.message_list_calls();
    hint(&mut client, &id, 1);
    let reads = client.peer_mut().inner.message_list_calls() - before;
    assert_eq!(
        reads,
        (unrelated + 2).div_ceil(50),
        "every page once, to the call"
    );
    assert_eq!(texts(&client).len(), shown, "nothing appended");
    let tools = tool_texts(&client);
    assert!(
        tools.len() == 1 && tools[0].contains("completed"),
        "{tools:?}"
    );
}

/// A prompt, one assistant record of two items, then exactly enough newer
/// one-slice rows that retention keeps only the record's NEWEST item.
fn partly_retained(record: Synthetic) -> Vec<Synthetic> {
    let mut rows = vec![Synthetic::User, record];
    rows.extend(std::iter::repeat_n(
        Synthetic::User,
        Bounds::PRODUCTION.max_retained_slices - 1,
    ));
    rows
}

#[test]
fn a_visible_call_in_a_partly_retained_record_is_refreshed() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, partly_retained(Synthetic::TextThenTool));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    let shown = texts(&client);
    assert!(
        !shown.iter().any(|text| text == "words 1") && tool_texts(&client).len() == 1,
        "the precondition: the record's text is evicted and its call shown"
    );
    client.peer_mut().inner.finish_tools();
    hint(&mut client, &id, 1);
    let tools = tool_texts(&client);
    assert!(
        tools.len() == 1 && tools[0].contains("completed"),
        "the visible call is updated in place: {tools:?}"
    );
    assert_eq!(texts(&client).len(), shown.len());
}

#[test]
fn a_call_just_outside_the_window_is_left_alone() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, partly_retained(Synthetic::ToolThenText));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    let shown = texts(&client);
    assert!(
        shown.first().map(String::as_str) == Some("words 1") && tool_texts(&client).is_empty(),
        "the precondition: the record's text is shown and its call evicted"
    );
    client.peer_mut().inner.finish_tools();
    hint(&mut client, &id, 1);
    assert!(
        tool_texts(&client).is_empty(),
        "the evicted call is not appended"
    );
    assert_eq!(texts(&client), shown);
}

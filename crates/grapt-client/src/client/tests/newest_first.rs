//! SC-HIST-01b: a session goes live on its newest page, and older history
//! follows behind it without ever deciding liveness, a terminal or lifetime
//! accounting.

use super::*;
use crate::double::Synthetic;

/// `rows` rows of whole turns: a prompt, then its reply.
fn turns(rows: usize) -> Vec<Synthetic> {
    (0..rows)
        .map(|index| {
            if index % 2 == 0 {
                Synthetic::User
            } else {
                Synthetic::Reply
            }
        })
        .collect()
}

/// A selected session whose durable history is `rows`, before any read of it.
fn seeded(bounds: Bounds, rows: Vec<Synthetic>) -> (Client<HeldPeer>, crate::wire::SessionWireId) {
    let mut client = Client::new(HeldPeer::new(), bounds);
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a selection: {error}");
    }
    let Ok(id) = client.create_session() else {
        panic!("create")
    };
    // The stream opens with its positioned connect before any history.
    let Ok(_) = client.pump() else { panic!("pump") };
    client.peer_mut().inner.seed_history(&id, rows);
    (client, id)
}

fn pump_until_settled(client: &mut Client<HeldPeer>) {
    for _ in 0..200 {
        if client.older_history() != &OlderHistory::Loading {
            return;
        }
        if let Err(error) = client.pump() {
            panic!("pump: {error}");
        }
    }
    panic!("the walk never stopped: {:?}", client.older_history());
}

/// Let the selection's own reads finish, then hold the next older page.
fn hold_older_page(client: &mut Client<HeldPeer>) {
    let Ok(_) = client.pump() else { panic!("pump") };
    client.peer_mut().hold = true;
    let Ok(_) = client.pump() else { panic!("pump") };
    let held = client.peer_mut().held.clone();
    assert!(
        held.as_ref()
            .is_some_and(|request| request.operation() == Operation::MessageList
                && request.query().iter().any(|(key, _)| key == "cursor")),
        "an older page is in flight: {held:?}"
    );
}

/// Whether the view's accounting claims the whole history.
fn whole_history_accounting(client: &Client<HeldPeer>) -> bool {
    client
        .view()
        .is_some_and(|view| !view.cache.status().contains("history incomplete"))
}

fn texts(client: &Client<HeldPeer>) -> Vec<String> {
    client
        .view()
        .map(|view| view.slices.iter().map(|slice| slice.text.clone()).collect())
        .unwrap_or_default()
}

/// Source rows plus a footer lifecycle cell after each completed reply.
fn expected_turns(rows: usize, window: usize) -> Vec<String> {
    (rows - window..rows)
        .flat_map(|index| {
            if index % 2 == 0 {
                vec![format!("turn {index}")]
            } else {
                vec![format!("reply {index}"), String::new()]
            }
        })
        .collect()
}

/// Retention bounds all slices, including the completed-reply footers.
fn expected_retained_turns(rows: usize, limit: usize) -> Vec<String> {
    let mut expected = expected_turns(rows, rows.min(limit));
    expected.split_off(expected.len().saturating_sub(limit))
}

/// The contract double behind a read lane that can HOLD a background read,
/// and a frame queue a witness drives.
#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct HeldPeer {
    inner: ContractDouble,
    /// Hold every read on the lane.
    hold: bool,
    /// Hold only continuations: older pages, never a newest page.
    hold_older: bool,
    /// Answer continuations with a body that is not a message page.
    corrupt_older: bool,
    held: Option<WireRequest>,
    cancels: usize,
    /// Newest-page reads, by either route.
    tail_reads: usize,
    closed: bool,
    frames: std::collections::VecDeque<FramePoll>,
}

fn continuation(request: &WireRequest) -> bool {
    request.query().iter().any(|(key, _)| key == "cursor")
}

impl HeldPeer {
    fn new() -> Self {
        Self {
            inner: ContractDouble::new(),
            hold: false,
            hold_older: false,
            corrupt_older: false,
            held: None,
            cancels: 0,
            tail_reads: 0,
            closed: false,
            frames: std::collections::VecDeque::new(),
        }
    }

    fn status(&mut self, session: &crate::wire::SessionWireId, kind: &str, created: u64) {
        let frame = serde_json::json!({
            "id": format!("evt_status_{created}"),
            "type": "session.status",
            "created": created,
            "data": {
                "sessionID": session.as_inner().as_str(),
                "status": { "type": kind },
                "state": {
                    "type": if kind == "busy" { "running" } else { "idle" },
                    "revision": created,
                    "executionID": "exe_1",
                },
            }
        });
        self.frames.push_back(FramePoll::Frame(frame.to_string()));
    }

    fn event(
        &mut self,
        session: &crate::wire::SessionWireId,
        seq: u64,
        kind: &str,
        mut data: serde_json::Value,
    ) {
        data["sessionID"] = serde_json::json!(session.as_inner().as_str());
        if kind.starts_with("session.text.") || kind.starts_with("session.reasoning.") {
            if let Some(fields) = data.as_object_mut() {
                fields
                    .entry("assistantMessageID")
                    .or_insert(serde_json::json!("msg_live_tail"));
                fields.entry("ordinal").or_insert(serde_json::json!(0));
            }
        }
        let mut frame = serde_json::json!({
            "id": format!("evt_live_{seq}"),
            "type": kind,
            "created": seq,
            "data": data,
        });
        if crate::contract::EventMember::parse(kind).is_some_and(|member| member.requires_durable())
        {
            frame["durable"] = serde_json::json!({
                "aggregateID": session.as_inner().as_str(), "seq": seq, "version": 1
            });
        }
        self.frames.push_back(FramePoll::Frame(frame.to_string()));
    }

    fn text(&mut self, session: &crate::wire::SessionWireId, seq: u64, kind: &str, delta: bool) {
        let data = if delta {
            serde_json::json!({ "delta": "LIVE TAIL" })
        } else if kind.ends_with(".ended") {
            serde_json::json!({ "text": "LIVE TAIL" })
        } else {
            serde_json::json!({})
        };
        self.event(session, seq, kind, data);
    }

    fn answer(
        &mut self,
        request: &WireRequest,
    ) -> Result<crate::transport::WireResponse, TransportError> {
        if request.operation() == Operation::SessionSnapshot {
            self.tail_reads += 1;
        }
        if request.operation() == Operation::MessageList {
            if !continuation(request) {
                self.tail_reads += 1;
            } else if self.corrupt_older {
                return Ok(crate::transport::WireResponse {
                    status: 200,
                    body: b"not a message page".to_vec(),
                });
            }
        }
        self.inner.call(request)
    }

    fn holds(&self, request: &WireRequest) -> bool {
        self.hold || (self.hold_older && continuation(request))
    }
}

impl Transport for HeldPeer {
    fn call(
        &mut self,
        request: &WireRequest,
    ) -> Result<crate::transport::WireResponse, TransportError> {
        self.answer(request)
    }
}

impl Connection for HeldPeer {
    fn subscribed(&self) -> bool {
        !self.closed
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        if let Some(connect) = self.inner.connect() {
            return Ok(connect);
        }
        match self.frames.pop_front() {
            Some(FramePoll::Closed) => {
                self.closed = true;
                Ok(FramePoll::Closed)
            }
            Some(frame) => Ok(self.inner.serve(frame)),
            None => Ok(FramePoll::Idle),
        }
    }
    fn resubscribe(&mut self) {
        self.closed = false;
        self.inner.resubscribe();
    }
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<crate::transport::WireResponse>, TransportError> {
        if self.holds(request) {
            self.held = Some(request.clone());
            return Ok(None);
        }
        self.answer(request).map(Some)
    }
    fn poll_read(&mut self) -> Result<Option<crate::transport::WireResponse>, TransportError> {
        match &self.held {
            Some(request) if self.holds(request) => return Ok(None),
            None => return Err(TransportError::NotConnected),
            Some(_) => {}
        }
        let request = self.held.take().ok_or(TransportError::NotConnected)?;
        self.answer(&request).map(Some)
    }
    fn cancel_read(&mut self) {
        if self.held.take().is_some() {
            self.cancels += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Time to live is one page, whatever the session's length.
// ---------------------------------------------------------------------------

#[test]
fn a_session_goes_live_on_one_page_however_long_it_is() {
    for rows in [100, 3_200, 20_000] {
        let (mut client, id) = seeded(Bounds::PRODUCTION, turns(rows));
        let before = client.peer_mut().tail_reads;
        let Ok(()) = client.reconcile(&id) else {
            panic!("reconcile {rows}")
        };
        assert_eq!(
            client.peer_mut().tail_reads - before,
            1,
            "{rows} rows: live after exactly one snapshot"
        );
        assert!(client.stream_state().is_live(), "{rows} rows: live");
        // Paint now, before any older page is read: the snapshot's rows show.
        assert_eq!(
            texts(&client),
            expected_turns(rows, rows.min(200)),
            "{rows} rows"
        );
        assert_eq!(client.older_history(), &OlderHistory::Loading);
    }
}

// ---------------------------------------------------------------------------
// The walk toward older history: its stops, and what it shows.
// ---------------------------------------------------------------------------

#[test]
fn backfill_fills_retention_in_reading_order_and_names_the_bound() {
    let rows = 20_000;
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(rows));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    assert_eq!(client.older_history(), &OlderHistory::Bounded);
    assert_eq!(
        texts(&client),
        expected_retained_turns(rows, 512),
        "the retained window is the one an oldest-first fold keeps"
    );
    assert!(client.stream_state().is_live());
    assert!(
        !whole_history_accounting(&client),
        "a bounded walk claims no lifetime accounting"
    );
}

#[test]
fn a_short_history_backfills_to_the_start() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(100));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    assert_eq!(client.older_history(), &OlderHistory::Complete);
    assert_eq!(texts(&client), expected_turns(100, 100));
    assert!(
        whole_history_accounting(&client),
        "a walk to the start covers the whole history"
    );
}

#[test]
fn byte_short_pages_are_followed_to_the_start() {
    // The product cuts a page at its encoded-byte bound and its cursor
    // continues: a short page is not the end.
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(100));
    client.peer_mut().inner.cut_pages_at(7);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    assert_eq!(client.older_history(), &OlderHistory::Complete);
    assert_eq!(texts(&client), expected_turns(100, 100));
}

#[test]
fn a_page_budget_spent_on_rows_that_draw_nothing_is_named_as_the_budget() {
    // Rows and slices are not interchangeable: a step with no content folds to
    // nothing. Spending the page budget with retention nowhere near full is
    // neither complete nor a retention bound.
    let mut rows = vec![Synthetic::User];
    rows.extend(std::iter::repeat_n(Synthetic::Empty, 20_000));
    let (mut client, id) = seeded(Bounds::PRODUCTION, rows);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    match client.older_history() {
        OlderHistory::Exhausted { pages, .. } => {
            assert_eq!(*pages, Bounds::PRODUCTION.max_message_pages)
        }
        other => panic!("expected the read budget, got {other:?}"),
    }
    assert!(texts(&client).is_empty(), "nothing drawable was read");
    assert!(client.stream_state().is_live());
}

#[test]
fn a_tail_that_fills_retention_reads_nothing_older() {
    let bounds = match (Bounds {
        max_retained_slices: 40,
        ..Bounds::PRODUCTION
    })
    .validate()
    {
        Ok(bounds) => bounds,
        Err(error) => panic!("bounds: {error}"),
    };
    let (mut client, id) = seeded(bounds, turns(1_000));
    let before = client.peer_mut().inner.message_list_calls();
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    for _ in 0..4 {
        let Ok(_) = client.pump() else { panic!("pump") };
    }
    assert_eq!(client.older_history(), &OlderHistory::Bounded);
    assert_eq!(
        client.peer_mut().inner.message_list_calls() - before,
        0,
        "no older page was requested"
    );
    assert_eq!(texts(&client), expected_retained_turns(1_000, 40));
}

// ---------------------------------------------------------------------------
// Ownership of the one read lane.
// ---------------------------------------------------------------------------

#[test]
fn a_live_frame_during_a_held_older_page_lands_on_the_tail() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(300));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    hold_older_page(&mut client);

    client
        .peer_mut()
        .text(&id, 501, "session.text.started", false);
    client.peer_mut().text(&id, 502, "session.text.delta", true);
    client
        .peer_mut()
        .text(&id, 503, "session.text.ended", false);
    let Ok(_) = client.pump() else { panic!("pump") };
    assert_eq!(
        texts(&client).last().map(String::as_str),
        Some("LIVE TAIL"),
        "a held older page does not hold back live frames"
    );

    client.peer_mut().hold = false;
    pump_until_settled(&mut client);
    let mut expected = expected_turns(300, 300);
    expected.push("LIVE TAIL".to_owned());
    assert_eq!(texts(&client), expected, "older rows went in front of it");
}

#[test]
fn switching_sessions_cancels_a_held_older_page() {
    let (mut client, first) = seeded(Bounds::PRODUCTION, turns(300));
    let Ok(()) = client.reconcile(&first) else {
        panic!("reconcile")
    };
    hold_older_page(&mut client);

    client.peer_mut().hold = false;
    let Ok(second) = client.create_session() else {
        panic!("second session")
    };
    assert_ne!(first, second);
    assert_eq!(client.peer_mut().cancels, 1, "the held page was cancelled");
    pump_until_settled(&mut client);
    assert!(
        texts(&client)
            .iter()
            .all(|text| !text.starts_with("turn ") && !text.starts_with("reply ")),
        "no row of the first session reached the second: {:?}",
        texts(&client)
    );
}

#[test]
fn a_restarted_tail_read_fences_out_the_previous_walk() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(300));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    hold_older_page(&mut client);
    let generation = client.recovery.generation;

    client.peer_mut().hold = false;
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile again")
    };
    assert_eq!(client.peer_mut().cancels, 1, "the held page was cancelled");
    assert_ne!(
        client.recovery.generation, generation,
        "a new read generation"
    );
    pump_until_settled(&mut client);
    assert_eq!(
        texts(&client),
        expected_turns(300, 300),
        "each row once: nothing from the cancelled walk was spliced"
    );
}

#[test]
fn a_foreground_read_takes_the_lane_and_the_walk_resumes_from_its_cursor() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(300));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    hold_older_page(&mut client);

    client.refresh_preset_display();
    let Ok(_) = client.pump() else { panic!("pump") };
    assert_eq!(client.peer_mut().cancels, 1, "the older page gave way");
    assert_eq!(
        client.peer_mut().held.as_ref().map(WireRequest::operation),
        Some(Operation::SessionPresetGet),
        "and the foreground read holds the lane"
    );

    client.peer_mut().hold = false;
    pump_until_settled(&mut client);
    assert_eq!(client.older_history(), &OlderHistory::Complete);
    assert_eq!(texts(&client), expected_turns(300, 300));
}

// ---------------------------------------------------------------------------
// R1′: a terminal is new by identity, decided by the tail alone.
// ---------------------------------------------------------------------------

fn busy(client: &mut Client<HeldPeer>, id: &crate::wire::SessionWireId, created: u64) {
    client.peer_mut().status(id, "busy", created);
    let Ok(_) = client.pump() else { panic!("pump") };
    assert_eq!(client.view().map(|view| view.idle), Some(Some(false)));
}

/// A turn's live terminal arrives, and the client re-reads the tail through
/// the recovery owner, as it does after every terminal.
fn finish_live(client: &mut Client<HeldPeer>, id: &crate::wire::SessionWireId) {
    let before = client.peer_mut().tail_reads;
    let execution = serde_json::json!({ "executionID": "exe_1" });
    client
        .peer_mut()
        .event(id, 900, "session.execution.started", execution.clone());
    client
        .peer_mut()
        .event(id, 901, "session.execution.succeeded", execution);
    for _ in 0..4 {
        let Ok(_) = client.pump() else { panic!("pump") };
    }
    assert_eq!(
        client.peer_mut().tail_reads,
        before + 1,
        "the terminal re-read the newest page once"
    );
}

fn settled(client: &Client<HeldPeer>) -> bool {
    matches!(
        client.view().map(|view| &view.execution),
        Some(ExecutionState::Settled { .. })
    )
}

#[test]
fn a_same_prompt_terminal_with_no_user_row_on_the_page_settles() {
    let mut rows = vec![Synthetic::User];
    rows.extend(std::iter::repeat_n(Synthetic::Step, 120));
    let (mut client, id) = seeded(Bounds::PRODUCTION, rows);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    busy(&mut client, &id, 801);

    client
        .peer_mut()
        .inner
        .extend_history(&id, &[Synthetic::Step, Synthetic::Reply]);
    finish_live(&mut client, &id);
    assert!(settled(&client), "the newest page carries the terminal");
}

#[test]
fn a_later_turn_finishing_after_backfill_to_retention_settles() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(2_000));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    assert_eq!(client.older_history(), &OlderHistory::Bounded);
    busy(&mut client, &id, 802);

    client
        .peer_mut()
        .inner
        .extend_history(&id, &[Synthetic::User, Synthetic::Step, Synthetic::Reply]);
    finish_live(&mut client, &id);
    assert!(settled(&client));
}

#[test]
fn backfill_alone_never_produces_a_terminal() {
    // An older window HOLDS a terminal; the tail is a turn in flight.
    let mut rows = vec![Synthetic::User, Synthetic::Reply, Synthetic::User];
    rows.extend(std::iter::repeat_n(Synthetic::Step, 60));
    let (mut client, id) = seeded(Bounds::PRODUCTION, rows);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    assert_eq!(client.older_history(), &OlderHistory::Complete);
    let Some(view) = client.view() else {
        panic!("view")
    };
    assert_eq!(view.execution, ExecutionState::Idle, "nothing settled");
    assert_eq!(view.held_terminal, None, "and no terminal is held");
}

#[test]
fn an_unchanged_reread_keeps_busy_observed_after_the_held_terminal() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(10));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    assert!(settled(&client));
    busy(&mut client, &id, 804);
    let Ok(()) = client.reconcile(&id) else {
        panic!("relist")
    };
    assert_eq!(client.view().map(|view| view.idle), Some(Some(false)));
}

#[test]
fn an_error_only_record_is_a_terminal() {
    let (mut client, id) = seeded(
        Bounds::PRODUCTION,
        vec![Synthetic::User, Synthetic::Step, Synthetic::Step],
    );
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    client
        .peer_mut()
        .inner
        .extend_history(&id, &[Synthetic::Errored]);
    let Ok(()) = client.reconcile(&id) else {
        panic!("relist")
    };
    assert_eq!(
        client
            .view()
            .and_then(|view| view.held_terminal.as_ref())
            .map(|(_, outcome)| outcome),
        Some(&Outcome::Failed)
    );
}

#[test]
fn a_tool_call_step_continues_the_turn_and_settles_nothing() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, vec![Synthetic::User, Synthetic::Reply]);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    let held = client.view().and_then(|view| view.held_terminal.clone());
    assert!(held.is_some(), "the first turn's terminal is held");
    client
        .peer_mut()
        .inner
        .extend_history(&id, &[Synthetic::User, Synthetic::Step]);
    let Ok(()) = client.reconcile(&id) else {
        panic!("relist")
    };
    client
        .peer_mut()
        .inner
        .extend_history(&id, &[Synthetic::Step]);
    let Ok(()) = client.reconcile(&id) else {
        panic!("relist")
    };
    assert_eq!(
        client.view().and_then(|view| view.held_terminal.clone()),
        held
    );
    assert_eq!(
        client.view().map(|view| &view.execution),
        Some(&ExecutionState::Idle),
        "a step that called a tool settles nothing"
    );
}

#[test]
fn a_later_row_with_no_lifecycle_does_not_hide_the_terminal() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, vec![Synthetic::User, Synthetic::Step]);
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    client
        .peer_mut()
        .inner
        .extend_history(&id, &[Synthetic::Reply, Synthetic::Notice]);
    let Ok(()) = client.reconcile(&id) else {
        panic!("relist")
    };
    let Some(reply) = crate::wire::MessageWireId::parse("msg_000000000003".to_owned()) else {
        panic!("id")
    };
    assert_eq!(
        client
            .view()
            .and_then(|view| view.held_terminal.as_ref())
            .map(|(id, _)| id),
        Some(&reply),
        "the terminal under the notice is the one held"
    );
}

// ---------------------------------------------------------------------------
// R2: older windows merge into accounting chronologically and once.
// ---------------------------------------------------------------------------

fn usage(input: Option<u64>, read: Option<u64>) -> crate::cache::CacheUsage {
    crate::cache::CacheUsage {
        input,
        read,
        ..crate::cache::CacheUsage::default()
    }
}

#[test]
fn an_older_window_never_overwrites_newer_accounting() {
    let mut older = crate::cache::CacheSummary::default();
    older.observe(usage(Some(100), Some(50)));
    let mut newer = crate::cache::CacheSummary::default();
    newer.observe(usage(Some(200), Some(200)));
    newer.prepend(older);
    assert_eq!(
        newer.latest,
        usage(Some(200), Some(200)),
        "latest stays newer"
    );
    assert_eq!(
        newer.miss_status(),
        Some("last observed miss: cause unreported".to_owned()),
        "the older miss fills a newer window that had none"
    );
    newer.history_complete(true);
    assert_eq!(
        newer.history_share(),
        Some(250.0 / 300.0),
        "counters add once"
    );

    // An UNREPORTED newer observation is still the newest word.
    let mut older = crate::cache::CacheSummary::default();
    older.observe(usage(Some(100), Some(50)));
    let mut newer = crate::cache::CacheSummary::default();
    newer.observe(usage(None, None));
    newer.prepend(older);
    assert_eq!(newer.latest, usage(None, None));
    newer.history_complete(true);
    assert_eq!(newer.history_share(), None, "and its unknown carries");
}

#[test]
fn a_live_observation_made_while_an_older_page_is_held_stays_newest() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(300));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    hold_older_page(&mut client);
    let live = usage(Some(1_000), Some(999));
    match client.views.get_mut(&id) {
        Some(view) => view.cache.observe(live),
        None => panic!("view"),
    }
    client.peer_mut().hold = false;
    pump_until_settled(&mut client);
    assert_eq!(client.view().map(|view| view.cache.latest), Some(live));
}

// ---------------------------------------------------------------------------
// The recovery owner: the tail never waits on older history.
// ---------------------------------------------------------------------------

#[test]
fn a_reconnect_installs_the_tail_and_live_frames_flow_while_an_older_page_is_held() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(300));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    pump_until_settled(&mut client);
    assert_eq!(client.older_history(), &OlderHistory::Complete);
    let reads = client.peer_mut().tail_reads;

    // The stream is cut; recovery re-reads the newest page on the lane, and
    // the older page behind it is held.
    client.peer_mut().hold_older = true;
    client.peer_mut().frames.push_back(FramePoll::Closed);
    for _ in 0..3 {
        let Ok(_) = client.pump() else { panic!("pump") };
    }
    assert_eq!(client.peer_mut().tail_reads, reads + 1, "one newest page");
    assert!(client.stream_state().is_live(), "live on the newest page");
    assert!(
        client.peer_mut().held.as_ref().is_some_and(continuation),
        "an older page is in flight"
    );
    assert_eq!(client.older_history(), &OlderHistory::Loading);
    assert_eq!(
        texts(&client),
        expected_turns(300, 300),
        "the scrollback stays on screen while the walk re-reads it"
    );

    client
        .peer_mut()
        .text(&id, 501, "session.text.started", false);
    client.peer_mut().text(&id, 502, "session.text.delta", true);
    client
        .peer_mut()
        .text(&id, 503, "session.text.ended", false);
    let Ok(_) = client.pump() else { panic!("pump") };
    assert_eq!(
        texts(&client).last().map(String::as_str),
        Some("LIVE TAIL"),
        "a live frame applies before the older page completes"
    );
    assert!(client.peer_mut().held.is_some(), "which is still held");

    client.peer_mut().hold_older = false;
    pump_until_settled(&mut client);
    assert_eq!(client.older_history(), &OlderHistory::Complete);
    let mut expected = expected_turns(300, 300);
    expected.push("LIVE TAIL".to_owned());
    assert_eq!(
        texts(&client),
        expected,
        "each row once, the live tail last"
    );
    assert!(
        client
            .view()
            .is_some_and(|view| view.cache.status().contains("150 messages unreported")),
        "each reply accounted once: {:?}",
        client.view().map(|view| view.cache.status())
    );
}

#[test]
fn an_unreadable_older_page_fails_only_the_older_history() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(300));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    let reads = client.peer_mut().tail_reads;
    client.peer_mut().corrupt_older = true;
    client.peer_mut().frames.push_back(FramePoll::Closed);
    for _ in 0..20 {
        if let Err(error) = client.pump() {
            panic!("an older page's refusal must not leave pump: {error}");
        }
    }
    assert!(client.stream_state().is_live(), "the tail stays usable");
    assert!(
        matches!(client.older_history(), OlderHistory::Failed { .. }),
        "{:?}",
        client.older_history()
    );
    assert!(!whole_history_accounting(&client));
    assert_eq!(
        client.peer_mut().tail_reads,
        reads + 1,
        "and the tail is not fetched again and again"
    );
    assert_eq!(texts(&client), expected_turns(300, 200));
}

// ---------------------------------------------------------------------------
// The double's cursor is anchored on a row, as the product's is.
// ---------------------------------------------------------------------------

#[test]
fn a_durable_append_while_an_older_page_is_held_shifts_nothing() {
    let (mut client, id) = seeded(Bounds::PRODUCTION, turns(300));
    let Ok(()) = client.reconcile(&id) else {
        panic!("reconcile")
    };
    hold_older_page(&mut client);
    client
        .peer_mut()
        .inner
        .extend_history(&id, &[Synthetic::User, Synthetic::Reply]);
    client
        .peer_mut()
        .text(&id, 501, "session.text.started", false);
    client.peer_mut().text(&id, 502, "session.text.delta", true);
    client
        .peer_mut()
        .text(&id, 503, "session.text.ended", false);
    let Ok(_) = client.pump() else { panic!("pump") };
    client.peer_mut().hold = false;
    pump_until_settled(&mut client);

    assert_eq!(client.older_history(), &OlderHistory::Complete);
    let mut expected = expected_turns(300, 300);
    expected.push("LIVE TAIL".to_owned());
    assert_eq!(
        texts(&client),
        expected,
        "no row repeated or skipped at the seam, and the live tail intact"
    );
    assert!(
        client
            .view()
            .is_some_and(|view| view.cache.status().contains("150 messages unreported")),
        "each reply accounted once: {:?}",
        client.view().map(|view| view.cache.status())
    );
}

mod hints;
mod identity;

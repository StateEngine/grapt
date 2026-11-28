use super::*;
use grapt_client::wire::{FormRow, FormState, SessionWireId};
use grapt_client::{
    Bounds, ContractDouble, FramePoll, Operation, Transport, TransportError, WireRequest,
    WireResponse,
};
use grapt_tui_forms::{Decision, Reply, Request, Target};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
/// The loop's form reads driven synchronously through the same pieces: the
/// due reads, the client's prepared request and its accept.
fn refresh(
    poll: &mut Poll,
    client: &mut Client<Peer>,
    now: Instant,
) -> std::result::Result<bool, ClientError> {
    let before = (client.forms().to_vec(), client.location_forms().to_vec());
    let mut failure = None;
    for read in poll.due(client, now, |_| false) {
        let applied = client
            .prepare_read(read)
            .and_then(|prepared| match prepared {
                Some(prepared) => {
                    let response = client.peer_mut().call(&prepared.request);
                    client.accept_read(prepared, response)
                }
                None => Ok(true),
            });
        if let Err(error) = poll.settled(read, applied, now) {
            failure = failure.or(Some(error));
        }
    }
    failure.map_or(Ok(()), Err)?;
    Ok(before != (client.forms().to_vec(), client.location_forms().to_vec()))
}
#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct Peer {
    inner: ContractDouble,
    writes: usize,
    reject_answer: bool,
    wrong_rejection_owner: bool,
    global: Option<(grapt_client::wire::ProjectRow, FormRow, FormState)>,
    lose_reply: bool,
    fail_state_after_write: bool,
    session_reads: usize,
    fail_session_reads: usize,
    location_reads: usize,
    fail_location_reads: usize,
    frames: std::collections::VecDeque<String>,
}
impl Transport for Peer {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn call(&mut self, request: &WireRequest) -> std::result::Result<WireResponse, TransportError> {
        if request.operation() == Operation::SessionFormList {
            self.session_reads += 1;
            if self.fail_session_reads > 0 {
                self.fail_session_reads -= 1;
                return Err(TransportError::Unavailable {
                    reason: "session read unavailable".into(),
                });
            }
        }
        if request.operation() == Operation::FormRequestList {
            self.location_reads += 1;
            if self.fail_location_reads > 0 {
                self.fail_location_reads -= 1;
                return Err(TransportError::Unavailable {
                    reason: "location read unavailable".into(),
                });
            }
        }
        if self.reject_answer
            && matches!(
                request.operation(),
                Operation::SessionFormReply | Operation::GlobalFormReply
            )
        {
            self.writes += 1;
            let form = if self.wrong_rejection_owner {
                "frm_foreign"
            } else {
                request
                    .path()
                    .trim_end_matches("/reply")
                    .rsplit('/')
                    .next()
                    .ok_or(TransportError::NotConnected)?
            };
            return Ok(WireResponse { status: 400, body: serde_json::to_vec(&serde_json::json!({"_tag":"FormInvalidAnswerError","id":form,"message":"Expected email for form field: branch"})).map_err(|error| TransportError::Io { reason: error.to_string() })? });
        }
        if let Some((project, row, state)) = self.global.as_mut() {
            use grapt_client::wire::*;
            if matches!(
                request.operation(),
                Operation::FormRequestList
                    | Operation::GlobalFormGet
                    | Operation::GlobalFormState
                    | Operation::GlobalFormReply
                    | Operation::GlobalFormCancel
            ) {
                assert!(request
                    .query()
                    .contains(&("location[directory]".into(), project.worktree.clone())));
                if request.operation() != Operation::FormRequestList {
                    assert!(request.path().starts_with("/api/session/global/form/"));
                }
                if request.operation() == Operation::GlobalFormState
                    && self.writes > 0
                    && self.fail_state_after_write
                {
                    return Err(TransportError::AfterSend {
                        reason: "state unavailable".into(),
                    });
                }
                let body = match request.operation() {
                    Operation::FormRequestList => encode_form_requests(
                        project,
                        if state.is_open() {
                            std::slice::from_ref(row)
                        } else {
                            &[]
                        },
                    ),
                    Operation::GlobalFormGet => encode_form(row),
                    Operation::GlobalFormState => encode_form_state(state),
                    Operation::GlobalFormReply | Operation::GlobalFormCancel => {
                        self.writes += 1;
                        *state = if request.operation() == Operation::GlobalFormCancel {
                            FormState::Cancelled
                        } else {
                            FormState::Answered(
                                decode_form_reply(
                                    request.body().ok_or(TransportError::NotConnected)?,
                                )
                                .map_err(|error| {
                                    TransportError::Io {
                                        reason: error.to_string(),
                                    }
                                })?,
                            )
                        };
                        if self.lose_reply {
                            return Err(TransportError::AfterSend {
                                reason: "lost global reply".into(),
                            });
                        }
                        encode_form_state(state)
                    }
                    _ => return Err(TransportError::NotConnected),
                }
                .map_err(|error| TransportError::Io {
                    reason: error.to_string(),
                })?;
                return Ok(WireResponse { status: 200, body });
            }
        }
        if matches!(
            request.operation(),
            Operation::SessionFormReply | Operation::SessionFormCancel
        ) {
            self.writes += 1;
            let response = self.inner.call(request)?;
            if self.lose_reply {
                return Err(TransportError::AfterSend {
                    reason: "lost form reply".into(),
                });
            }
            return Ok(response);
        }
        if request.operation() == Operation::SessionFormState
            && self.writes > 0
            && self.fail_state_after_write
        {
            return Err(TransportError::AfterSend {
                reason: "state unavailable".into(),
            });
        }
        self.inner.call(request)
    }
}
impl Connection for Peer {
    fn poll_frame(&mut self) -> std::result::Result<FramePoll, TransportError> {
        if let Some(connect) = self.inner.connect() {
            return Ok(connect);
        }
        if let Some(frame) = self.frames.pop_front() {
            return Ok(self.inner.serve(FramePoll::Frame(frame)));
        }
        self.inner.poll_frame()
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
}
/// Run one form transaction to the end on the test connection.
fn execute(client: &mut Client<Peer>, request: Request) -> Reply {
    let turn = begin(client, request);
    crate::backend::command_lane::run(client, turn, Flow::advance)
}

fn fixture() -> Result<(Client<Peer>, SessionWireId, FormRow)> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            writes: 0,
            reject_answer: false,
            wrong_rejection_owner: false,
            global: None,
            lose_reply: false,
            fail_state_after_write: false,
            session_reads: 0,
            fail_session_reads: 0,
            location_reads: 0,
            fail_location_reads: 0,
            frames: std::collections::VecDeque::new(),
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().ok_or("project")?.clone();
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client.models().first().ok_or("model")?.clone();
    client.select_model(&model)?;
    let session = client.create_session()?;
    client
        .peer_mut()
        .inner
        .raise_form(&session, "branch?")
        .ok_or("raise")?;
    client.refresh_forms()?;
    let row = client.forms().first().ok_or("form")?.clone();
    Ok((client, session, row))
}
/// The stream's positioned connect, then the snapshot it permits.
fn prove(client: &mut Client<Peer>) -> Result {
    for _ in 0..2 {
        client.pump()?;
    }
    Ok(())
}
#[test]
fn session_notifications_discover_late_forms_without_an_idle_timer() -> Result {
    let (mut client, session, mut row) = fixture()?;
    prove(&mut client)?;
    let now = Instant::now();
    let mut poll = Poll::new(now);
    refresh(&mut poll, &mut client, now)?;
    assert_eq!(poll.wait(now), None);
    let id = client
        .peer_mut()
        .inner
        .raise_form(&session, "late question")
        .ok_or("late")?;
    row.id = id;
    row.title = "late question".into();
    let form: serde_json::Value = serde_json::from_slice(&grapt_client::wire::encode_form(&row)?)?;
    let event = serde_json::json!({"type":"form.created","id":"evt_000000000001",
        "created":1,"durable":{"aggregateID":session,"seq":1_000,"version":1},
        "data":{"sessionID":session,"form":form["data"]}})
    .to_string();
    client.peer_mut().frames.extend([event.clone(), event]);
    let reads = client.peer_mut().session_reads;
    assert!(client.pump()?);
    assert!(refresh(&mut poll, &mut client, now)?);
    assert_eq!(client.forms().len(), 2);
    assert_eq!(client.peer_mut().session_reads, reads + 1);
    assert_eq!(poll.wait(now), None);
    assert!(!refresh(&mut poll, &mut client, now + INTERVAL * 10)?);
    assert_eq!(client.peer_mut().session_reads, reads + 1);
    Ok(())
}

#[test]
fn partial_attachment_retries_failed_session_reads_and_discovers_location_forms() -> Result {
    let (mut client, session, mut row) = fixture()?;
    let now = Instant::now();
    let mut poll = Poll::new(now);
    refresh(&mut poll, &mut client, now)?;
    row.owner = grapt_client::wire::FormOwner::Global;
    let project = client.project().ok_or("project")?.clone();
    client.peer_mut().global = Some((project, row.clone(), FormState::Pending));
    client.peer_mut().fail_session_reads = 2;
    assert!(client.select(&session).is_err());
    assert!(refresh(&mut poll, &mut client, now).is_err());
    assert_eq!(client.location_forms(), &[row]);
    assert_eq!(poll.wait(now), Some(INTERVAL));
    assert!(!refresh(&mut poll, &mut client, now + INTERVAL / 2)?);
    assert!(refresh(&mut poll, &mut client, now + INTERVAL)?);
    assert_eq!(poll.wait(now + INTERVAL), None);
    assert_eq!(client.forms().len(), 1);
    Ok(())
}

#[test]
fn idle_does_not_repeat_session_or_location_discovery() -> Result {
    let (mut client, _, _) = fixture()?;
    let now = Instant::now();
    let mut poll = Poll::new(now);
    refresh(&mut poll, &mut client, now)?;
    let reads = (
        client.peer_mut().session_reads,
        client.peer_mut().location_reads,
    );
    for interval in 1..=30 {
        refresh(&mut poll, &mut client, now + INTERVAL * interval)?;
    }
    assert_eq!(
        (
            client.peer_mut().session_reads,
            client.peer_mut().location_reads
        ),
        reads
    );
    assert_eq!(poll.wait(now + INTERVAL * 30), None);
    // Opening another Session reestablishes its location list without waiting
    // for another form event, even when the old location had no pending forms.
    client.create_session()?;
    refresh(&mut poll, &mut client, now + INTERVAL * 30)?;
    assert_eq!(client.peer_mut().location_reads, reads.1 + 1);
    Ok(())
}

#[test]
fn global_form_hints_coalesce_and_failed_reads_retry_within_the_bound() -> Result {
    let (mut client, session, mut row) = fixture()?;
    prove(&mut client)?;
    let now = Instant::now();
    let mut poll = Poll::new(now);
    refresh(&mut poll, &mut client, now)?;
    let reads = client.peer_mut().location_reads;
    row.owner = grapt_client::wire::FormOwner::Global;
    let form: serde_json::Value = serde_json::from_slice(&grapt_client::wire::encode_form(&row)?)?;
    let project = client.project().ok_or("project")?.clone();
    client.peer_mut().global = Some((project, row.clone(), FormState::Pending));
    let event = serde_json::json!({"type":"form.created","id":"evt_000000000001",
        "location":{"directory":client.form_location().ok_or("location")?.directory},
        "created":1,"durable":{"aggregateID":row.id,"seq":1,"version":2},
        "data":{"sessionID":"global","form":form["data"]}})
    .to_string();
    client.peer_mut().frames.extend([event.clone(), event]);
    assert!(client.pump()?);
    assert_eq!(client.selected(), Some(&session));
    client.peer_mut().fail_location_reads = 1;
    assert!(refresh(&mut poll, &mut client, now).is_err());
    assert_eq!(client.peer_mut().location_reads, reads + 1);
    assert_eq!(poll.wait(now), Some(INTERVAL));
    assert!(!refresh(&mut poll, &mut client, now + INTERVAL / 2)?);
    assert_eq!(client.peer_mut().location_reads, reads + 1);
    refresh(&mut poll, &mut client, now + INTERVAL)?;
    assert_eq!(client.peer_mut().location_reads, reads + 2);
    assert_eq!(client.location_forms(), &[row.clone()]);
    if let Some((_, _, state)) = client.peer_mut().global.as_mut() {
        *state = FormState::Cancelled;
    }
    let event = serde_json::json!({"type":"form.cancelled","id":"evt_000000000002",
        "location":{"directory":client.form_location().ok_or("location")?.directory},
        "created":2,"durable":{"aggregateID":row.id,"seq":2,"version":2},
        "data":{"sessionID":"global","id":row.id}})
    .to_string();
    client.peer_mut().frames.push_back(event);
    assert!(client.pump()?);
    refresh(
        &mut poll,
        &mut client,
        now + INTERVAL + Duration::from_millis(1),
    )?;
    assert!(client.location_forms().is_empty());
    assert_eq!(client.peer_mut().location_reads, reads + 3);
    refresh(&mut poll, &mut client, now + INTERVAL * 2)?;
    assert_eq!(client.peer_mut().location_reads, reads + 3);
    Ok(())
}
#[test]
fn canonical_state_reconciles_a_committed_reply_and_blocks_stale_session_writes() -> Result {
    let (mut client, session, row) = fixture()?;
    client.peer_mut().lose_reply = true;
    let request = Request {
        target: Target::Session(session.clone()),
        form: row,
        decision: Some(Decision::Cancel),
    };
    let result = execute(&mut client, request.clone());
    assert_eq!(result.state, Some(FormState::Cancelled));
    assert_eq!(client.peer_mut().writes, 1);
    let result = execute(&mut client, request.clone());
    assert_eq!(result.state, Some(FormState::Cancelled));
    assert_eq!(client.peer_mut().writes, 1);
    client.create_session()?;
    let result = execute(&mut client, request);
    assert!(result.note.contains("Session changed"));
    assert_eq!(client.peer_mut().writes, 1);
    Ok(())
}
#[test]
fn missing_state_keeps_uncertainty_and_a_read_only_check_never_writes() -> Result {
    let (mut client, session, row) = fixture()?;
    client.peer_mut().lose_reply = true;
    client.peer_mut().fail_state_after_write = true;
    let mut request = Request {
        target: Target::Session(session),
        form: row,
        decision: Some(Decision::Cancel),
    };
    let result = execute(&mut client, request.clone());
    assert!(result.state.is_none());
    assert!(result.note.contains("unconfirmed"));
    request.decision = None;
    assert!(execute(&mut client, request.clone()).state.is_none());
    assert_eq!(client.peer_mut().writes, 1);
    client.peer_mut().fail_state_after_write = false;
    assert_eq!(
        execute(&mut client, request).state,
        Some(FormState::Cancelled)
    );
    assert_eq!(client.peer_mut().writes, 1);
    Ok(())
}

#[test]
fn global_lost_reply_reads_recorded_state_without_replaying_or_claiming_live_delivery() -> Result {
    let (mut client, _, mut row) = fixture()?;
    row.owner = grapt_client::wire::FormOwner::Global;
    let project = client.project().ok_or("project")?.clone();
    let location = client.form_location().ok_or("location")?;
    client.peer_mut().global = Some((
        project,
        row.clone(),
        FormState::Suspended(grapt_client::wire::FormSuspension::ProcessLoss),
    ));
    client.refresh_location_forms()?;
    assert_eq!(client.location_forms(), &[row.clone()]);
    client.peer_mut().lose_reply = true;
    client.peer_mut().fail_state_after_write = true;
    let mut request = Request {
        target: Target::Location(location.clone()),
        form: row,
        decision: Some(Decision::Cancel),
    };
    assert!(execute(&mut client, request.clone())
        .note
        .contains("unconfirmed"));
    request.decision = None;
    assert!(execute(&mut client, request.clone()).state.is_none());
    assert_eq!(client.peer_mut().writes, 1);
    client.peer_mut().fail_state_after_write = false;
    assert_eq!(
        execute(&mut client, request.clone()).state,
        Some(FormState::Cancelled)
    );
    assert_eq!(client.peer_mut().writes, 1);
    request.target = Target::Location(grapt_client::wire::FormLocation {
        directory: "/other".into(),
        ..location
    });
    request.decision = Some(Decision::Cancel);
    assert!(execute(&mut client, request)
        .note
        .contains("Location changed"));
    assert_eq!(client.peer_mut().writes, 1);
    Ok(())
}
#[test]
fn a_suspended_global_wait_can_record_a_decision_but_cannot_resume_its_old_waiter() -> Result {
    let (mut client, _, mut row) = fixture()?;
    row.owner = grapt_client::wire::FormOwner::Global;
    let project = client.project().ok_or("project")?.clone();
    let location = client.form_location().ok_or("location")?;
    client.peer_mut().global = Some((
        project,
        row.clone(),
        FormState::Suspended(grapt_client::wire::FormSuspension::Shutdown),
    ));
    let result = execute(
        &mut client,
        Request {
            target: Target::Location(location),
            form: row,
            decision: Some(Decision::Cancel),
        },
    );
    assert_eq!(result.state, Some(FormState::Cancelled));
    assert!(result.note.contains("suspended MCP wait was not resumed"));
    assert_eq!(client.peer_mut().writes, 1);
    Ok(())
}

#[test]
fn global_reads_refuse_scope_and_owner_changes_and_fresh_schema_changes_block_writes() -> Result {
    let (mut client, _, session_row) = fixture()?;
    let mut row = session_row.clone();
    row.owner = grapt_client::wire::FormOwner::Global;
    let project = client.project().ok_or("project")?.clone();
    let location = client.form_location().ok_or("location")?;
    client.peer_mut().global = Some((project, row.clone(), FormState::Pending));
    client.refresh_location_forms()?;
    assert_eq!(client.forms(), &[session_row]);
    let request = Request {
        target: Target::Location(location.clone()),
        form: row.clone(),
        decision: Some(Decision::Cancel),
    };
    if let Some((project, actual, _)) = client.peer_mut().global.as_mut() {
        project.id = grapt_client::wire::project_id("prj_other")?;
        actual.owner = grapt_client::wire::FormOwner::Unattributed("unknown".into());
    }
    assert!(client.refresh_location_forms().is_err());
    assert!(client.location_form(&location, &row.id).is_err());
    assert!(execute(&mut client, request.clone()).state.is_none());
    if let Some((_, actual, _)) = client.peer_mut().global.as_mut() {
        actual.owner = grapt_client::wire::FormOwner::Global;
        actual.fields[0].title = Some("Changed meaning".into());
    }
    assert!(execute(&mut client, request)
        .note
        .contains("Question changed"));
    assert_eq!(client.peer_mut().writes, 0);
    Ok(())
}

#[test]
fn only_a_matching_validation_receipt_allows_correction_without_replaying() -> Result {
    for global in [false, true] {
        let (mut client, session, mut row) = fixture()?;
        let target = if global {
            row.owner = grapt_client::wire::FormOwner::Global;
            client.peer_mut().global = Some((
                client.project().ok_or("project")?.clone(),
                row.clone(),
                FormState::Pending,
            ));
            Target::Location(client.form_location().ok_or("location")?)
        } else {
            Target::Session(session)
        };
        client.peer_mut().reject_answer = true;
        let request = Request {
            target,
            form: row,
            decision: Some(Decision::Answer(std::collections::BTreeMap::from([
                (
                    "branch".into(),
                    grapt_client::wire::FormValue::Text("bad".into()),
                ),
                ("dry_run".into(), grapt_client::wire::FormValue::Flag(true)),
            ]))),
        };
        client.peer_mut().wrong_rejection_owner = true;
        let reply = execute(&mut client, request.clone());
        assert!(!reply.validation_refused);
        client.peer_mut().wrong_rejection_owner = false;
        let reply = execute(&mut client, request.clone());
        assert!(reply.validation_refused && reply.note.contains("Expected email"));
        assert_eq!(reply.state, Some(FormState::Pending));
        let writes = client.peer_mut().writes;
        let mut check = request.clone();
        check.decision = None;
        assert!(!execute(&mut client, check).validation_refused);
        assert_eq!(client.peer_mut().writes, writes);
        client.peer_mut().fail_state_after_write = true;
        // Clear the attempt counter only to let this fixture's next preflight read succeed.
        client.peer_mut().writes = 0;
        let reply = execute(&mut client, request);
        assert!(reply.validation_refused && reply.state.is_none());
        assert!(reply.note.contains("state unreadable"));
    }
    Ok(())
}

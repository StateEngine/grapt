use super::*;
use crate::client::Connection;
use crate::transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse};
use crate::Operation;

const QUESTION: &str = "How does the TUI feel?";

#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct Peer {
    inner: ContractDouble,
    pending: bool,
    steering: bool,
    fail_pending: bool,
    promoted: bool,
    frames: std::collections::VecDeque<String>,
}

impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if request.operation() == Operation::SessionPendingList {
            if self.fail_pending {
                return Ok(WireResponse {
                    status: 503,
                    body: Vec::new(),
                });
            }
            let session = request
                .path()
                .split('/')
                .nth(3)
                .ok_or(TransportError::NotConnected)?;
            let rows = if self.pending {
                let user =
                    serde_json::from_value(serde_json::json!({"type":"user","admittedSeq":1,
                    "id":"msg_saved_question","sessionID":session,"timeCreated":1,
                    "data":{"text":QUESTION},"delivery":if self.steering { "steer" } else { "queue" }}))
                    .map_err(|_| TransportError::NotConnected)?;
                vec![grapt_api::u3a::SessionPendingInfoDto::User(Box::new(user))]
            } else {
                Vec::new()
            };
            let page =
                grapt_api::u3a::SessionPendingListResponseDto::new(grapt_schema::Required::new(
                    grapt_api::u3a::BoundedVec::try_new(rows)
                        .map_err(|_| TransportError::NotConnected)?,
                ));
            return Ok(WireResponse {
                status: 200,
                body: grapt_api::u3a::encode_page(
                    &page,
                    crate::wire::limits().map_err(|_| TransportError::NotConnected)?,
                )
                .map_err(|_| TransportError::NotConnected)?,
            });
        }
        if request.operation() == Operation::MessageList && self.promoted {
            let rows = (|| -> Result<_, crate::wire::WireError> {
                Ok(vec![
                    crate::wire::assistant_message(
                        "msg_answer",
                        &[crate::wire::MessageItem::Text(
                            "answer still arriving".into(),
                        )],
                        None,
                        None,
                        None,
                    )?,
                    crate::wire::user_message("msg_saved_question", QUESTION)?,
                ])
            })()
            .map_err(|_| TransportError::NotConnected)?;
            return Ok(WireResponse {
                status: 200,
                body: crate::wire::encode_messages(rows, None)
                    .map_err(|_| TransportError::NotConnected)?,
            });
        }
        self.inner.call(request)
    }
}

impl Connection for Peer {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        if let Some(frame) = self.frames.pop_front() {
            Ok(self.inner.serve(FramePoll::Frame(frame)))
        } else {
            self.inner.poll_frame()
        }
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_saved_question_stays_visible_until_it_enters_the_transcript(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            pending: false,
            steering: false,
            fail_pending: false,
            promoted: false,
            frames: std::collections::VecDeque::new(),
        },
        Bounds::PRODUCTION,
    );
    client.select_first_offer()?;
    let session = client.create_session()?;
    for _ in 0..4 {
        client.pump()?;
    }
    client.peer_mut().pending = true;
    for (kind, data) in [
        (
            "session.text.started",
            serde_json::json!({"assistantMessageID":"msg_fixture_answer","ordinal":0,"sessionID":session.spelled()}),
        ),
        (
            "session.text.delta",
            serde_json::json!({"assistantMessageID":"msg_fixture_answer","ordinal":0,"sessionID":session.spelled(),"delta":"still arriving"}),
        ),
        (
            "session.input.admitted",
            serde_json::json!({"sessionID":session.spelled()}),
        ),
    ] {
        let mut frame = serde_json::json!({
            "id":format!("evt_{}",kind.replace('.', "_")),"type":kind,"created":1,"data":data
        });
        if crate::contract::EventMember::parse(kind).is_some_and(|event| event.requires_durable()) {
            frame["durable"] =
                serde_json::json!({"aggregateID":session.spelled(),"seq":100,"version":1});
        }
        crate::contract::EventEnvelope::decode(&frame.to_string())?;
        client.peer_mut().frames.push_back(frame.to_string());
    }
    for _ in 0..4 {
        client.pump()?;
    }
    let view = client.view().ok_or("view")?;
    let before = super::super::queued_input_lines(&view.queued, 72).join("\n");
    assert!(
        before.contains(QUESTION),
        "saved question disappeared: {before}"
    );
    assert!(before.contains("Queued"));
    assert!(view
        .slices
        .iter()
        .any(|slice| slice.text.contains("still arriving")));
    client.peer_mut().fail_pending = true;
    client.reconcile(&session)?;
    for _ in 0..4 {
        client.pump()?;
    }
    let unreadable =
        super::super::queued_input_lines(&client.view().ok_or("view")?.queued, 72).join("\n");
    assert!(unreadable.contains(QUESTION));
    assert!(client.view().ok_or("view")?.queued.unavailable.is_some());
    assert!(client.stream_state().is_live());
    client.peer_mut().fail_pending = false;
    client.peer_mut().pending = false;
    client.peer_mut().promoted = true;
    let reconciliations = client.diagnostics().reconciliations;
    for frame in [
        serde_json::json!({"id":"evt_answer_start","type":"session.text.started","created":2,
            "durable":{"aggregateID":session.spelled(),"seq":200,"version":1},
            "data":{"assistantMessageID":"msg_fixture_answer","ordinal":1,"sessionID":session.spelled()}}),
        serde_json::json!({"id":"evt_answer_delta","type":"session.text.delta","created":2,
            "data":{"assistantMessageID":"msg_fixture_answer","ordinal":1,"sessionID":session.spelled(),"delta":"answer still arriving"}}),
        serde_json::json!({"id":"evt_question_promoted","type":"session.input.promoted","created":2,
            "durable":{"aggregateID":session.spelled(),"seq":201,"version":1},
            "data":{"sessionID":session.spelled(),"inputID":"msg_saved_question"}}),
    ] {
        crate::contract::EventEnvelope::decode(&frame.to_string())?;
        client.peer_mut().frames.push_back(frame.to_string());
    }
    for _ in 0..4 {
        client.pump()?;
    }
    let view = client.view().ok_or("view")?;
    let after = super::super::queued_input_lines(&view.queued, 72).join("\n");
    assert!(!after.contains("Queued"));
    assert_eq!(client.diagnostics().reconciliations, reconciliations);
    assert_eq!(
        view.slices
            .iter()
            .filter(|slice| slice.text.contains("answer still arriving"))
            .count(),
        1
    );
    assert_eq!(
        client
            .view()
            .ok_or("view")?
            .slices
            .iter()
            .filter(|row| row.kind == SliceKind::User && row.text == QUESTION)
            .count(),
        1
    );
    Ok(())
}

#[test]
fn a_composer_steer_is_not_classified_as_a_queued_followup(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            pending: true,
            steering: true,
            fail_pending: false,
            promoted: false,
            frames: std::collections::VecDeque::new(),
        },
        Bounds::PRODUCTION,
    );
    client.select_first_offer()?;
    let session = client.create_session()?;
    for _ in 0..4 {
        client.pump()?;
    }
    let pending = &client.view().ok_or("view")?.queued;
    assert_eq!(pending.total, 0, "plain Enter was classified as queued");
    assert!(pending.messages.is_empty());
    client.peer_mut().steering = false;
    client.reconcile(&session)?;
    for _ in 0..4 {
        client.pump()?;
    }
    let pending = &client.view().ok_or("view")?.queued;
    assert_eq!(pending.total, 1);
    assert_eq!(pending.messages, [QUESTION]);
    Ok(())
}

//! Lost replies while admission is queued, independent of history projection.
use super::*;
use crate::transport::{Transport, WireRequest, WireResponse};

#[derive(Debug, Clone, Copy, Default)]
enum Read {
    #[default]
    Data,
    Empty,
    Malformed,
    Unavailable,
    WrongSession,
    WrongKind,
    WrongText,
    WrongDelivery,
    OverBound,
    TooLarge,
    Paginated,
}

#[derive(Debug, Default)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct QueuedPeer {
    inner: ContractDouble,
    admissions: Vec<WireRequest>,
    attempts: Vec<WireRequest>,
    pending: Read,
    history: Read,
    lose_reply: bool,
    reply_error: Option<TransportError>,
    reply_status: Option<u16>,
    drop_before_admission: bool,
    unsent: bool,
    refuse: bool,
    promoted: bool,
    promote_after_pending: bool,
    reads: Vec<Operation>,
}

impl QueuedPeer {
    fn promote(&mut self) {
        if self.promoted {
            return;
        }
        for request in &self.admissions {
            let session = request
                .path()
                .trim_start_matches("/api/session/")
                .trim_end_matches("/prompt");
            let session = crate::wire::session_id(session)
                .unwrap_or_else(|error| panic!("fixture session: {error}"));
            let bytes = request.body().unwrap_or_default();
            let text = crate::wire::decode_prompt(bytes)
                .unwrap_or_else(|error| panic!("fixture prompt: {error}"));
            let id = crate::wire::decode_prompt_id(bytes)
                .unwrap_or_else(|error| panic!("fixture id: {error}"));
            self.inner.run_turn_as(&session, &text, "one answer", id);
        }
        self.promoted = true;
    }

    fn pending_page(&self, request: &WireRequest) -> Vec<u8> {
        let mut rows = Vec::new();
        if !self.promoted && !matches!(self.pending, Read::Empty | Read::Paginated) {
            for admitted in &self.admissions {
                if admitted.path().trim_end_matches("/prompt")
                    != request.path().trim_end_matches("/pending")
                {
                    continue;
                }
                let bytes = admitted.body().unwrap_or_default();
                let text = crate::wire::decode_prompt(bytes)
                    .unwrap_or_else(|error| panic!("fixture prompt: {error}"));
                let id = crate::wire::decode_prompt_id(bytes)
                    .unwrap_or_else(|error| panic!("fixture id: {error}"));
                let session = request
                    .path()
                    .trim_start_matches("/api/session/")
                    .trim_end_matches("/pending");
                let payload: serde_json::Value = serde_json::from_slice(bytes)
                    .unwrap_or_else(|error| panic!("fixture input: {error}"));
                rows.push(serde_json::json!({
                    "id":id,"sessionID":if matches!(self.pending,Read::WrongSession) { "ses_other" } else { session },
                    "type":if matches!(self.pending,Read::WrongKind) { "synthetic" } else { "user" },
                    "admittedSeq":1,"timeCreated":1,"delivery":if matches!(self.pending,Read::WrongDelivery) { if payload["delivery"] == "steer" { "queue" } else { "steer" } } else { payload["delivery"].as_str().unwrap_or("queue") },
                    "data":{"text":if matches!(self.pending,Read::WrongText) { "unrelated" } else { &text }},
                }));
                if let Some(metadata) = payload.get("metadata") {
                    let row = rows
                        .last_mut()
                        .unwrap_or_else(|| panic!("pending row missing"));
                    row["data"]["metadata"] = metadata.clone();
                }
            }
        }
        if matches!(self.pending, Read::OverBound) {
            let row = rows
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("overbound fixture needs an admitted row"));
            rows = vec![row; 129];
        }
        let bytes = serde_json::to_vec(&serde_json::json!({"data":rows}))
            .unwrap_or_else(|error| panic!("fixture JSON: {error}"));
        if matches!(self.pending, Read::OverBound) {
            return bytes;
        }
        // All positive/negative fixtures speak the real DTO, including wrong
        // scope/kind/text rows. Malformed fixtures are injected separately.
        let limits =
            crate::wire::limits().unwrap_or_else(|error| panic!("production limits: {error}"));
        let raw: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("fixture JSON: {error}"));
        let rows = raw["data"]
            .as_array()
            .unwrap_or_else(|| panic!("fixture data array"))
            .iter()
            .cloned()
            .map(|row| {
                if row["type"] == "synthetic" {
                    grapt_api::u3a::SessionPendingInfoDto::Synthetic(Box::new(
                        serde_json::from_value(row)
                            .unwrap_or_else(|error| panic!("fixture synthetic DTO: {error}")),
                    ))
                } else {
                    grapt_api::u3a::SessionPendingInfoDto::User(Box::new(
                        serde_json::from_value(row)
                            .unwrap_or_else(|error| panic!("fixture user DTO: {error}")),
                    ))
                }
            })
            .collect();
        let dto = grapt_api::u3a::SessionPendingListResponseDto::new(grapt_schema::Required::new(
            grapt_api::u3a::BoundedVec::try_new(rows)
                .unwrap_or_else(|error| panic!("fixture row bound: {error}")),
        ));
        grapt_api::u3a::encode_page(&dto, limits)
            .unwrap_or_else(|error| panic!("fixture pending encode: {error}"))
    }
}

impl Transport for QueuedPeer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if request.operation() == Operation::SessionPrompt {
            assert!(self.attempts.len() < 8, "bounded prompt fixture");
            self.attempts.push(request.clone());
            if self.unsent {
                self.unsent = false;
                return Err(TransportError::NotConnected);
            }
            if self.refuse {
                self.refuse = false;
                return Ok(WireResponse {
                    status: 400,
                    body: Vec::new(),
                });
            }
            if !self.drop_before_admission && !self.admissions.contains(request) {
                self.admissions.push(request.clone());
            }
            if self.lose_reply {
                self.lose_reply = false;
                self.drop_before_admission = false;
                if let Some(status) = self.reply_status.take() {
                    return Ok(WireResponse {
                        status,
                        body: Vec::new(),
                    });
                }
                return Err(self
                    .reply_error
                    .take()
                    .unwrap_or(TransportError::AfterSend {
                        reason: "fixture reply lost".to_owned(),
                    }));
            }
            return Ok(WireResponse {
                status: 200,
                body: Vec::new(),
            });
        }
        let mode = match request.operation() {
            Operation::SessionPendingList => self.pending,
            Operation::MessageList => self.history,
            _ => return self.inner.call(request),
        };
        assert!(self.reads.len() < 64, "bounded read fixture");
        self.reads.push(request.operation());
        let body = match mode {
            Read::Unavailable => {
                return Err(TransportError::Unavailable {
                    reason: "private response must not escape".to_owned(),
                })
            }
            Read::Malformed => b"PRIVATE_RESPONSE_NOT_JSON".to_vec(),
            Read::TooLarge => vec![b'x'; 2048],
            Read::Empty | Read::Paginated if request.operation() == Operation::MessageList => {
                crate::wire::encode_messages(
                    Vec::new(),
                    matches!(mode, Read::Paginated).then_some("more"),
                )
                .unwrap_or_else(|error| panic!("fixture message page: {error}"))
            }
            _ if request.operation() == Operation::SessionPendingList => self.pending_page(request),
            _ => return self.inner.call(request),
        };
        if request.operation() == Operation::SessionPendingList && self.promote_after_pending {
            self.promote();
        }
        Ok(WireResponse { status: 200, body })
    }
}

impl Connection for QueuedPeer {
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
}

fn queued() -> (Client<QueuedPeer>, crate::wire::SessionWireId) {
    let peer = QueuedPeer {
        lose_reply: true,
        ..QueuedPeer::default()
    };
    let mut client = Client::new(peer, Bounds::PRODUCTION)
        .with_input_ids(std::sync::Arc::new(CountingIds::default()));
    client
        .select_first_offer()
        .unwrap_or_else(|error| panic!("selection: {error}"));
    let session = client
        .create_session()
        .unwrap_or_else(|error| panic!("session: {error}"));
    client.connection.reads.clear();
    (client, session)
}

#[test]
fn a_queued_admission_recovers_a_lost_reply_before_history_promotion() {
    let (mut client, session) = queued();
    assert_eq!(client.submit_prompt("first"), Ok(()));
    assert!(client.take_recovered());
    assert!(client.pending.is_empty());
    assert_eq!(client.connection.admissions.len(), 1);
    let sent: serde_json::Value =
        serde_json::from_slice(client.connection.admissions[0].body().unwrap_or_default())
            .unwrap_or_else(|error| panic!("prompt JSON: {error}"));
    assert_eq!(
        sent["delivery"], "steer",
        "composer input must reach the next safe boundary"
    );
    assert_eq!(client.connection.reads, [Operation::SessionPendingList]);
    assert!(
        client.connection.inner.user_inputs(&session).is_empty(),
        "not promoted yet"
    );
    client.connection.promote();
    assert_eq!(
        client.connection.inner.user_inputs(&session),
        [Some("msg_witness0".to_owned())]
    );
}

#[test]
fn promotion_between_reconciliation_reads_is_found_in_history() {
    let (mut client, _) = queued();
    client.connection.pending = Read::Empty;
    client.connection.promote_after_pending = true;
    assert_eq!(client.submit_prompt("first"), Ok(()));
    assert!(client.take_recovered());
    assert_eq!(client.connection.admissions.len(), 1);
    assert_eq!(
        client.connection.reads,
        [Operation::SessionPendingList, Operation::MessageList]
    );
}

#[test]
fn missing_or_unreadable_evidence_never_replaces_an_uncertain_identity() {
    for mode in [
        Read::Empty,
        Read::Malformed,
        Read::Unavailable,
        Read::WrongSession,
        Read::WrongKind,
        Read::WrongText,
        Read::WrongDelivery,
        Read::OverBound,
        Read::TooLarge,
        Read::Paginated,
    ] {
        let (mut client, session) = queued();
        client.bounds.max_response_bytes = if matches!(mode, Read::OverBound) {
            1024 * 1024
        } else {
            1024
        };
        client.connection.pending = mode;
        client.connection.history = if matches!(
            mode,
            Read::Malformed | Read::Unavailable | Read::TooLarge | Read::Paginated
        ) {
            mode
        } else {
            Read::Empty
        };
        assert_eq!(
            client.submit_prompt("PRIVATE_ORIGINAL"),
            Err(ClientError::Unconfirmed),
            "{mode:?}"
        );
        let error = client.submit_prompt("PRIVATE_EDIT");
        assert_eq!(error, Err(ClientError::UnconfirmedEdit), "{mode:?}");
        assert_eq!(client.connection.attempts.len(), 1, "edited text was sent");
        assert_eq!(client.pending.len(), 1);
        let debug = format!("{:?}", client.pending);
        assert!(!debug.contains("PRIVATE_ORIGINAL"));
        assert!(!error
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default()
            .contains("PRIVATE"));
        assert_eq!(client.retry_prompt("PRIVATE_EDIT"), Ok(false));
        assert_eq!(
            client.connection.admissions.len(),
            1,
            "exact retry duplicated admission"
        );
        assert_eq!(client.connection.attempts[0], client.connection.attempts[1]);
        client.connection.promote();
        assert_eq!(client.connection.inner.user_inputs(&session).len(), 1);
    }
}

#[test]
fn an_edit_that_confirms_the_original_is_kept_for_a_separate_send() {
    let (mut client, _) = queued();
    client.connection.pending = Read::Unavailable;
    client.connection.history = Read::Unavailable;
    assert_eq!(client.submit_prompt("first"), Err(ClientError::Unconfirmed));
    client.connection.pending = Read::Data;
    assert_eq!(
        client.submit_prompt("edited"),
        Err(ClientError::EarlierSubmitted)
    );
    assert!(client.pending.is_empty());
    assert_eq!(client.connection.attempts.len(), 1);
    assert_eq!(client.submit_prompt("edited"), Ok(()));
    assert_eq!(client.connection.admissions.len(), 2);
}

#[test]
fn definite_unsent_or_refused_drafts_remain_editable() {
    for refused in [false, true] {
        let (mut client, _) = queued();
        client.connection.lose_reply = false;
        client.connection.unsent = !refused;
        client.connection.refuse = refused;
        assert!(client.submit_prompt("first").is_err());
        assert!(client.pending.is_empty());
        assert_eq!(client.submit_prompt("edited"), Ok(()));
        assert_eq!(client.connection.admissions.len(), 1);
        assert_eq!(
            crate::wire::decode_prompt(client.connection.admissions[0].body().unwrap_or_default()),
            Ok("edited".to_owned())
        );
    }
}

#[test]
fn a_refused_retry_does_not_erase_the_earlier_uncertain_submission() {
    let (mut client, _) = queued();
    client.connection.pending = Read::Empty;
    client.connection.history = Read::Empty;
    assert_eq!(client.submit_prompt("first"), Err(ClientError::Unconfirmed));
    client.connection.refuse = true;
    assert!(client.retry_prompt("edited").is_err());
    assert_eq!(client.pending.len(), 1);
    assert_eq!(
        client.submit_prompt("edited"),
        Err(ClientError::UnconfirmedEdit)
    );
    assert_eq!(client.retry_prompt("edited"), Ok(false));
    assert_eq!(client.connection.admissions.len(), 1);
}

#[test]
fn an_unavailable_identity_source_refuses_before_sending() {
    let (mut client, _) = queued();
    client.input_ids = None;
    assert_eq!(
        client.submit_prompt("first"),
        Err(ClientError::InputIdentityUnavailable)
    );
    assert!(client.connection.attempts.is_empty());
}

#[test]
fn an_unreadable_or_server_error_reply_retains_the_same_retry_identity() {
    for kind in 0..3 {
        let (mut client, _) = queued();
        client.connection.pending = Read::Empty;
        client.connection.history = Read::Empty;
        match kind {
            0 => {
                client.connection.reply_error =
                    Some(TransportError::ResponseTooLarge { bound: 1024 })
            }
            1 => {
                client.connection.reply_error = Some(TransportError::NotUtf8 {
                    reason: "private bytes".to_owned(),
                })
            }
            _ => client.connection.reply_status = Some(500),
        }
        assert_eq!(client.submit_prompt("first"), Err(ClientError::Unconfirmed));
        assert_eq!(
            client.submit_prompt("edit"),
            Err(ClientError::UnconfirmedEdit)
        );
        assert_eq!(client.retry_prompt("edit"), Ok(false));
        assert_eq!(client.connection.admissions.len(), 1);
        assert_eq!(client.connection.attempts[0], client.connection.attempts[1]);
    }
}

#[test]
fn valid_history_can_confirm_despite_an_unreadable_pending_response() {
    let (mut client, _) = queued();
    client.connection.pending = Read::Malformed;
    client.connection.history = Read::Unavailable;
    assert_eq!(client.submit_prompt("first"), Err(ClientError::Unconfirmed));
    client.connection.promote();
    client.connection.history = Read::Data;
    assert_eq!(
        client.submit_prompt("edited"),
        Err(ClientError::EarlierSubmitted)
    );
    assert_eq!(client.connection.attempts.len(), 1);
    assert!(client.pending.is_empty());
}

#[test]
fn uncertain_images_require_an_exact_wire_retry_instead_of_text_only_confirmation(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut client, _) = queued();
    let image: grapt_api::u3a::PromptInputFileAttachmentDto = serde_json::from_value(
        serde_json::json!({"uri":"data:image/png;base64,aW1hZ2U=","name":"image.png"}),
    )?;
    let different: grapt_api::u3a::PromptInputFileAttachmentDto = serde_json::from_value(
        serde_json::json!({"uri":"data:image/png;base64,b3RoZXI=","name":"other.png"}),
    )?;
    assert_eq!(
        client.submit_prompt_with_files("first", std::slice::from_ref(&image)),
        Err(ClientError::Unconfirmed)
    );
    assert!(client.has_unconfirmed_prompt());
    assert!(
        client.connection.reads.is_empty(),
        "text-only pending/history is not proof of the image"
    );
    assert_eq!(
        client.submit_prompt_with_files("first", &[different]),
        Err(ClientError::UnconfirmedEdit)
    );
    assert_eq!(client.connection.attempts.len(), 1);
    assert_eq!(client.retry_prompt("edited"), Ok(false));
    assert!(!client.has_unconfirmed_prompt());
    assert_eq!(client.connection.attempts.len(), 2);
    assert_eq!(
        client.connection.attempts[0].body(),
        client.connection.attempts[1].body()
    );
    assert_eq!(client.connection.admissions.len(), 1);
    Ok(())
}

#[test]
fn a_prepared_startup_keeps_wire_origin_across_an_uncertain_retry(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut client, _session) = queued();
    client.connection.pending = Read::Unavailable;
    client.connection.history = Read::Unavailable;
    assert!(client.submit_seat_startup("packet").is_err());
    let first = client.connection.attempts[0]
        .body()
        .ok_or("first body")?
        .to_vec();
    let body: serde_json::Value = serde_json::from_slice(&first)?;
    assert_eq!(
        body["metadata"],
        serde_json::json!({"origin":"seat-startup"})
    );
    client.retry_prompt("")?;
    assert_eq!(client.connection.attempts[1].body(), Some(first.as_slice()));
    assert_eq!(client.connection.admissions.len(), 1);
    Ok(())
}

#[test]
fn identical_composer_text_does_not_turn_an_automated_retry_into_human_input() {
    for startup in [false, true] {
        let (mut client, _) = queued();
        client.connection.pending = Read::Unavailable;
        client.connection.history = Read::Unavailable;
        let result = if startup {
            client.submit_seat_startup("identical text")
        } else {
            client.submit_prompt("identical text")
        };
        assert_eq!(result, Err(ClientError::Unconfirmed));
        assert_eq!(client.retry_prompt("identical text"), Ok(!startup));
        assert_eq!(client.connection.admissions.len(), 1);
        assert_eq!(client.connection.attempts[0], client.connection.attempts[1]);
    }
}

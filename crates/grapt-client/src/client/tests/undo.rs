use super::*;
use crate::transport::{FramePoll, Transport, WireResponse};
#[derive(Debug)]
struct UndoConnection {
    inner: ContractDouble,
    lose: usize,
    conflicts: usize,
    commits: Vec<Vec<u8>>,
    status: &'static str,
}
impl Transport for UndoConnection {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        let operation = request.operation();
        if !matches!(
            operation,
            Operation::SessionUndoPreview | Operation::SessionUndo
        ) {
            return self.inner.call(request);
        }
        let session =
            request
                .path()
                .split('/')
                .nth(3)
                .ok_or_else(|| TransportError::Unavailable {
                    reason: "Session missing".into(),
                })?;
        let body = if operation == Operation::SessionUndoPreview {
            serde_json::json!({"data":{"sessionID":session,"inputID":"msg_undo_fixture","target":"checkpoint_fixture","paths":["a.txt","b.txt"],"incompletePatches":0,"writes":1,"deletes":1,"bytes":8}})
        } else {
            self.commits
                .push(request.body().unwrap_or_default().to_vec());
            if self.lose > 0 {
                self.lose -= 1;
                return Err(TransportError::AfterSend {
                    reason: "injected lost file-undo reply".into(),
                });
            }
            if self.conflicts > 0 {
                self.conflicts -= 1;
                return Ok(WireResponse {
                    status: 409,
                    body: serde_json::json!({"_tag":"ConflictError","message":"The pending input prevents reconciliation.","resource":"execution"}).to_string().into_bytes(),
                });
            }
            serde_json::json!({"data":{"sessionID":session,"inputID":"msg_undo_fixture","target":"checkpoint_fixture","status":self.status,"message":"fixture journal outcome"}})
        };
        Ok(WireResponse {
            status: 200,
            body: body.to_string().into_bytes(),
        })
    }
}
impl Connection for UndoConnection {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
}
fn fixture(lose: usize, status: &'static str) -> Client<UndoConnection> {
    let mut client = Client::new(
        UndoConnection {
            inner: ContractDouble::new(),
            lose,
            conflicts: 0,
            commits: Vec::new(),
            status,
        },
        Bounds::PRODUCTION,
    );
    client
        .select_first_offer()
        .unwrap_or_else(|error| panic!("offer: {error}"));
    client
        .create_session()
        .unwrap_or_else(|error| panic!("Session: {error}"));
    client
}
#[test]
fn pu_undo_requires_a_preview_and_retries_only_its_exact_target() {
    let mut client = fixture(1, "applied");
    assert!(client.apply_file_undo().is_err());
    client
        .preview_file_undo()
        .unwrap_or_else(|error| panic!("preview: {error}"));
    assert!(client.visible_file_undo().is_some());
    let receipt = client
        .apply_file_undo()
        .unwrap_or_else(|error| panic!("apply: {error}"));
    assert_eq!(
        receipt.status,
        grapt_api::u3a::SessionUndoStatusDto::Applied
    );
    assert!(client.visible_file_undo().is_none());
    assert_eq!(client.connection.commits.len(), 2);
    assert_eq!(client.connection.commits[0], client.connection.commits[1]);
}
#[test]
fn pu_lost_undo_replies_block_prompts_until_the_same_journal_receipt_arrives() {
    let mut client = fixture(2, "quarantined");
    client
        .preview_file_undo()
        .unwrap_or_else(|error| panic!("preview: {error}"));
    assert!(matches!(
        client.apply_file_undo(),
        Err(ClientError::UndoUnconfirmed)
    ));
    assert_eq!(
        client.submit_prompt("new edits"),
        Err(ClientError::UndoUnconfirmed)
    );
    assert!(matches!(
        client.preview_file_undo(),
        Err(ClientError::UndoUnconfirmed)
    ));
    assert!(matches!(
        client.rewind(1),
        Err(ClientError::UndoUnconfirmed)
    ));
    let receipt = client
        .apply_file_undo()
        .unwrap_or_else(|error| panic!("reconcile: {error}"));
    assert_eq!(
        receipt.status,
        grapt_api::u3a::SessionUndoStatusDto::Quarantined
    );
    assert!(client
        .connection
        .commits
        .windows(2)
        .all(|calls| calls[0] == calls[1]));
    assert!(!client.undo_uncertain);
}

#[test]
fn pu_undo_refusal_preserves_its_cause_without_hiding_uncertain_server_failures() {
    let client = fixture(0, "applied");
    let body = serde_json::json!({"_tag":"ConflictError","message":"File undo unavailable: the capture baseline changed.","resource":"execution"}).to_string().into_bytes();
    for operation in [Operation::SessionUndoPreview, Operation::SessionUndo] {
        let error = client.accept_response(
            operation,
            WireResponse {
                status: 409,
                body: body.clone(),
            },
        );
        assert!(
            matches!(error,Err(ClientError::Refused {message,..}) if message.contains("capture baseline changed"))
        );
        let uncertain = client.accept_response(
            operation,
            WireResponse {
                status: 500,
                body: body.clone(),
            },
        );
        assert!(matches!(
            uncertain,
            Err(ClientError::Status { status: 500, .. })
        ));
        assert!(uncertain
            .as_ref()
            .is_err_and(crate::client::submission::uncertain));
    }
}

#[test]
fn pu_undo_retry_conflict_does_not_disprove_the_lost_requests_effects() {
    let mut client = fixture(1, "applied");
    client.connection.conflicts = 1;
    client
        .preview_file_undo()
        .unwrap_or_else(|error| panic!("preview: {error}"));
    assert!(matches!(
        client.apply_file_undo(),
        Err(ClientError::UndoUnconfirmed)
    ));
    assert_eq!(
        client.submit_prompt("new edits"),
        Err(ClientError::UndoUnconfirmed)
    );
    assert!(matches!(
        client.preview_file_undo(),
        Err(ClientError::UndoUnconfirmed)
    ));
    let receipt = client
        .apply_file_undo()
        .unwrap_or_else(|error| panic!("reconcile: {error}"));
    assert_eq!(
        receipt.status,
        grapt_api::u3a::SessionUndoStatusDto::Applied
    );
    assert!(!client.undo_uncertain);
    assert_eq!(client.connection.commits.len(), 3);
    assert!(client
        .connection
        .commits
        .windows(2)
        .all(|calls| calls[0] == calls[1]));

    let mut known_refusal = fixture(0, "applied");
    known_refusal.connection.conflicts = 1;
    known_refusal
        .preview_file_undo()
        .unwrap_or_else(|error| panic!("preview: {error}"));
    assert!(matches!(
        known_refusal.apply_file_undo(),
        Err(ClientError::Refused { status: 409, .. })
    ));
    assert!(
        !known_refusal.undo_uncertain,
        "a definitive first refusal introduced uncertainty"
    );
}

/// SC-REL-01b: a prompt refused because a crash lost a tool run, and a
/// refused `/reconcile`, show their cause, which names the way out.
#[test]
fn a_prompt_refused_by_a_lost_tool_run_names_reconcile() {
    let client = fixture(0, "applied");
    let body = serde_json::json!({"_tag":"ConflictError","message":"an earlier tool run was lost before its result was recorded; run /reconcile to record it as unknown and continue","resource":"execution"}).to_string().into_bytes();
    for operation in [Operation::SessionPrompt, Operation::SessionReconcile] {
        let error = client.accept_response(
            operation,
            WireResponse {
                status: 409,
                body: body.clone(),
            },
        );
        assert!(
            matches!(&error, Err(ClientError::Refused { message, .. }) if message.contains("run /reconcile")),
            "{operation:?}: {error:?}"
        );
    }
}

use super::*;
use crate::transport::{FramePoll, Transport, WireResponse};

#[derive(Debug)]
struct LostRewind {
    inner: ContractDouble,
    lose: usize,
    commits: Vec<Vec<u8>>,
    refuse_history: bool,
    corrupt_history: bool,
}
impl Transport for LostRewind {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if matches!(
            request.operation(),
            Operation::MessageList | Operation::SessionSnapshot
        ) {
            if self.refuse_history {
                return Err(TransportError::Unavailable {
                    reason: "service reconnecting".into(),
                });
            }
            if self.corrupt_history {
                return Ok(WireResponse {
                    status: 200,
                    body: b"not JSON".to_vec(),
                });
            }
        }
        let response = self.inner.call(request)?;
        if request.operation() == Operation::SessionRewind {
            self.commits
                .push(request.body().unwrap_or_default().to_vec());
            if self.lose > 0 {
                self.lose -= 1;
                return Err(TransportError::AfterSend {
                    reason: "injected lost rewind response".into(),
                });
            }
        }
        Ok(response)
    }
}
impl Connection for LostRewind {
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

fn fixture(lose: usize) -> Client<LostRewind> {
    let mut client = Client::new(
        LostRewind {
            inner: ContractDouble::new(),
            lose,
            commits: Vec::new(),
            refuse_history: false,
            corrupt_history: false,
        },
        Bounds::PRODUCTION,
    );
    client
        .select_first_offer()
        .unwrap_or_else(|error| panic!("select: {error}"));
    client
        .create_session()
        .unwrap_or_else(|error| panic!("create: {error}"));
    client
        .submit_prompt("keep this")
        .unwrap_or_else(|error| panic!("first prompt: {error}"));
    client
        .pump()
        .unwrap_or_else(|error| panic!("first response: {error}"));
    client
        .submit_prompt("reopen this")
        .unwrap_or_else(|error| panic!("second prompt: {error}"));
    client
        .pump()
        .unwrap_or_else(|error| panic!("second response: {error}"));
    client
}

#[test]
fn rw_lost_reply_retries_the_same_cut_once_and_rebuilds_history() {
    let mut client = fixture(1);
    let edit = client
        .rewind(1)
        .unwrap_or_else(|error| panic!("rewind: {error}"));
    assert_eq!(edit.prompt, "reopen this");
    assert_eq!(edit.history, RewindHistory::Refreshed);
    assert_eq!(client.connection.commits.len(), 2);
    assert_eq!(client.connection.commits[0], client.connection.commits[1]);
    let session = client
        .selected()
        .cloned()
        .unwrap_or_else(|| panic!("missing session"));
    assert_eq!(client.connection.inner.user_inputs(&session).len(), 1);
}

#[test]
fn rw_unknown_outcome_requires_reload_before_more_edits_or_prompts() {
    let mut client = fixture(2);
    assert!(matches!(
        client.rewind(1),
        Err(ClientError::RewindUnconfirmed)
    ));
    assert!(matches!(
        client.rewind(1),
        Err(ClientError::RewindUnconfirmed)
    ));
    assert_eq!(
        client.submit_prompt("new draft"),
        Err(ClientError::RewindUnconfirmed)
    );
    assert_eq!(client.connection.commits.len(), 2);
    let session = client
        .selected()
        .cloned()
        .unwrap_or_else(|| panic!("missing session"));
    client
        .select(&session)
        .unwrap_or_else(|error| panic!("reload: {error}"));
    assert_eq!(client.connection.inner.user_inputs(&session).len(), 1);
    client
        .submit_prompt("new draft")
        .unwrap_or_else(|error| panic!("new prompt: {error}"));
    assert_eq!(client.connection.inner.user_inputs(&session).len(), 2);
}

#[test]
fn rw_reconnecting_history_reloads_without_another_cut_or_idle_timer() {
    let mut client = fixture(0);
    browse_before_rewind(&mut client);
    let revision = client.history_revision();
    let session = client
        .selected()
        .cloned()
        .unwrap_or_else(|| panic!("session"));
    let retained = client
        .connection
        .inner
        .user_inputs(&session)
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("retained input identity"));
    client.connection.refuse_history = true;
    let edit = client.rewind(1).unwrap_or_else(|e| panic!("rewind: {e}"));
    assert_eq!(edit.prompt, "reopen this");
    assert_eq!(edit.history, RewindHistory::Recovering);
    assert!(!client.browsing_history());
    assert!(client.history_revision() > revision);
    assert_eq!(client.history_edges(), (false, false));
    assert!(
        client.display_view().is_none(),
        "the stale reading page must be gone"
    );
    assert!(client.recovery.history.is_some());
    assert!(client.recovery_wait().is_some());
    assert!(
        client.view().is_none(),
        "old conversation must not survive the cut"
    );
    client.connection.refuse_history = false;
    client
        .pump()
        .unwrap_or_else(|e| panic!("history recovery: {e}"));
    assert_rewound_transcript(&client);
    assert!(client.recovery.history.is_none());
    assert_eq!(
        client.recovery_wait(),
        None,
        "a healthy client has no recovery timer"
    );
    assert_eq!(client.connection.commits.len(), 1);
    let session = client
        .selected()
        .cloned()
        .unwrap_or_else(|| panic!("session"));
    assert_eq!(
        client.connection.inner.user_inputs(&session),
        vec![retained]
    );
}

#[test]
fn rw_malformed_history_reports_failure_and_preserves_the_reopened_prompt() {
    let mut client = fixture(0);
    client.connection.corrupt_history = true;
    let edit = client.rewind(1).unwrap_or_else(|e| panic!("rewind: {e}"));
    assert_eq!(edit.prompt, "reopen this");
    assert!(matches!(edit.history, RewindHistory::Failed(_)));
    assert!(client.recovery.history.is_none());
    assert_eq!(client.connection.commits.len(), 1);
}

fn browse_before_rewind(client: &mut Client<LostRewind>) {
    client.enable_paged_history();
    assert!(client
        .browse_history(crate::client::HistoryMove::Beginning)
        .unwrap_or_else(|e| panic!("browse: {e}")));
    assert!(client.browsing_history());
    let view = client
        .display_view()
        .unwrap_or_else(|| panic!("reading page"));
    assert!(view
        .slices
        .iter()
        .any(|slice| slice.text.contains("reopen this")));
}

fn assert_rewound_transcript(client: &Client<LostRewind>) {
    assert!(!client.browsing_history());
    let view = client
        .display_view()
        .unwrap_or_else(|| panic!("reloaded transcript"));
    assert!(view
        .slices
        .iter()
        .any(|slice| slice.text.contains("keep this")));
    assert!(!view
        .slices
        .iter()
        .any(|slice| slice.text.contains("reopen this")));
}

#[test]
fn rw_immediate_history_refresh_replaces_the_browsed_page_at_the_confirmed_cut() {
    let mut client = fixture(0);
    browse_before_rewind(&mut client);
    let revision = client.history_revision();
    let edit = client.rewind(1).unwrap_or_else(|e| panic!("rewind: {e}"));
    assert_eq!(edit.prompt, "reopen this");
    assert_eq!(edit.history, RewindHistory::Refreshed);
    assert!(client.history_revision() > revision);
    assert_rewound_transcript(&client);
    assert_eq!(client.connection.commits.len(), 1);
}

//! A dry run (BACKLOG "Dry run, pre-Train-51"): the prompt's reply was
//! lost after the server admitted it, the client showed an error and kept the
//! draft, and resubmitting would have run the task twice. Every prompt now
//! carries an input id; a lost reply is reconciled from the session, and a
//! resubmission is an exact retry, never a second task.

use super::*;

/// Deterministic input ids, as every witness takes them.
#[derive(Debug, Default)]
struct CountingIds(std::sync::atomic::AtomicU64);

impl grapt_types::IdAuthority for CountingIds {
    fn mint(&self) -> grapt_types::IdToken {
        let next = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        grapt_types::IdToken::new(format!("witness{next}"))
            .unwrap_or_else(|| panic!("a counted id is never empty"))
    }
}

/// A client over the double, in a fresh session, minting counted ids.
fn submitting() -> (Client<ContractDouble>, crate::wire::SessionWireId) {
    let mut client = client().with_input_ids(std::sync::Arc::new(CountingIds::default()));
    let Ok(_) = client.create_session() else {
        panic!("create");
    };
    let Some(session) = client.selected().cloned() else {
        panic!("the created session is selected");
    };
    (client, session)
}

#[test]
fn a_prompt_carries_the_input_id_it_was_drafted_under() {
    let (mut client, session) = submitting();
    let Ok(()) = client.submit_prompt("hello") else {
        panic!("prompt");
    };
    assert_eq!(
        client.connection.user_inputs(&session),
        vec![Some("msg_witness0".to_owned())]
    );
    assert!(!client.take_recovered());
}

#[test]
fn an_uncertain_queued_followup_retries_once_without_clearing_equal_human_text() {
    let (mut client, session) = submitting();
    client
        .connection
        .set_fault(Fault::PromptReplyLostUnconfirmed);
    assert_eq!(
        client.submit_queued_prompt("next task", &[]),
        Err(ClientError::Unconfirmed)
    );
    assert_eq!(client.retry_prompt("next task"), Ok(false));
    assert_eq!(
        client.connection.user_inputs(&session),
        vec![Some("msg_witness0".to_owned())]
    );
    assert!(!client.has_unconfirmed_prompt());
}

#[test]
fn a_lost_reply_the_session_shows_admitted_is_a_submission() {
    let (mut client, session) = submitting();
    client.connection.set_fault(Fault::PromptReplyLost);
    assert_eq!(client.submit_prompt("do the task"), Ok(()));
    assert!(client.take_recovered(), "the recovery was not reported");
    assert!(!client.take_recovered(), "the recovery is reported once");
    assert_eq!(
        client.connection.user_inputs(&session),
        vec![Some("msg_witness0".to_owned())],
        "one admission"
    );
}

#[test]
fn an_unconfirmed_prompt_is_kept_and_its_resubmission_runs_once() {
    let (mut client, session) = submitting();
    // Admitted, reply lost, and the confirming read fails too: the client
    // cannot know, so it keeps the draft and says resubmitting is safe.
    client
        .connection
        .set_fault(Fault::PromptReplyLostUnconfirmed);
    assert_eq!(
        client.submit_prompt("do the task"),
        Err(ClientError::Unconfirmed)
    );
    assert_eq!(client.submit_prompt("do the task"), Ok(()));
    assert_eq!(
        client.connection.user_inputs(&session),
        vec![Some("msg_witness0".to_owned())],
        "the resubmission ran the task again"
    );
}

#[test]
fn a_prompt_lost_before_admission_is_kept_and_sent_once_on_retry() {
    let (mut client, session) = submitting();
    client
        .connection
        .set_fault(Fault::PromptLostBeforeAdmission);
    assert_eq!(
        client.submit_prompt("do the task"),
        Err(ClientError::Unconfirmed)
    );
    assert!(client.connection.user_inputs(&session).is_empty());
    assert_eq!(client.submit_prompt("do the task"), Ok(()));
    assert_eq!(
        client.connection.user_inputs(&session),
        vec![Some("msg_witness0".to_owned())]
    );
}

#[test]
fn an_edit_keeps_the_unresolved_original_until_explicit_retry() {
    let (mut client, session) = submitting();
    client
        .connection
        .set_fault(Fault::PromptLostBeforeAdmission);
    assert_eq!(client.submit_prompt("first"), Err(ClientError::Unconfirmed));
    assert_eq!(
        client.submit_prompt("first, edited"),
        Err(ClientError::UnconfirmedEdit)
    );
    assert!(client.connection.user_inputs(&session).is_empty());
    assert_eq!(client.retry_prompt("first, edited"), Ok(false));
    assert_eq!(
        client.connection.user_inputs(&session),
        vec![Some("msg_witness0".to_owned())]
    );
    assert_eq!(client.submit_prompt("first, edited"), Ok(()));
    assert_eq!(client.connection.user_inputs(&session).len(), 2);
}

/// A client in session A whose prompt went unconfirmed, with a second
/// session B to switch to (review SS-H1).
fn unconfirmed_in_a_with_b() -> (
    Client<ContractDouble>,
    crate::wire::SessionWireId,
    crate::wire::SessionWireId,
) {
    let (mut client, a) = submitting();
    let Ok(b) = client.create_session() else {
        panic!("create B");
    };
    let Ok(()) = client.select(&a) else {
        panic!("select A");
    };
    client
        .connection
        .set_fault(Fault::PromptReplyLostUnconfirmed);
    assert_eq!(
        client.submit_prompt("same text"),
        Err(ClientError::Unconfirmed)
    );
    (client, a, b)
}

#[test]
fn the_same_text_in_another_session_is_a_new_input() {
    let (mut client, a, b) = unconfirmed_in_a_with_b();
    let Ok(()) = client.select(&b) else {
        panic!("select B");
    };
    assert_eq!(client.submit_prompt("same text"), Ok(()));
    assert_eq!(
        client.connection.user_inputs(&a),
        vec![Some("msg_witness0".to_owned())]
    );
    assert_eq!(
        client.connection.user_inputs(&b),
        vec![Some("msg_witness1".to_owned())],
        "B reused A's input id"
    );
}

#[test]
fn a_prompt_sent_in_another_session_keeps_the_unconfirmed_retry() {
    let (mut client, a, b) = unconfirmed_in_a_with_b();
    let Ok(()) = client.select(&b) else {
        panic!("select B");
    };
    assert_eq!(client.submit_prompt("different text"), Ok(()));
    let Ok(()) = client.select(&a) else {
        panic!("return to A");
    };
    assert_eq!(client.submit_prompt("same text"), Ok(()));
    assert_eq!(
        client.connection.user_inputs(&a),
        vec![Some("msg_witness0".to_owned())],
        "A's retry ran the task again"
    );
}

/// Every prompt under one id, to reach the server's rule directly.
#[derive(Debug)]
struct OneId;

impl grapt_types::IdAuthority for OneId {
    fn mint(&self) -> grapt_types::IdToken {
        grapt_types::IdToken::new("only".to_owned())
            .unwrap_or_else(|| panic!("a fixed id is never empty"))
    }
}

#[test]
fn the_double_refuses_an_input_id_another_session_holds() {
    let mut client = client().with_input_ids(std::sync::Arc::new(OneId));
    let (Ok(a), Ok(b)) = (client.create_session(), client.create_session()) else {
        panic!("create");
    };
    assert_eq!(client.submit_prompt("same text"), Ok(()));
    let Ok(()) = client.select(&a) else {
        panic!("select A");
    };
    assert!(
        client.submit_prompt("same text").is_err(),
        "the product refuses an input id held by another session; the double must too"
    );
    assert!(client.connection.user_inputs(&a).is_empty());
    assert_eq!(
        client.connection.user_inputs(&b),
        vec![Some("msg_only".to_owned())]
    );
}

#[test]
fn an_interruption_keeps_the_input_id_and_its_global_admission() {
    let mut client = client().with_input_ids(std::sync::Arc::new(OneId));
    let a = client
        .create_session()
        .unwrap_or_else(|error| panic!("create A: {error}"));
    client
        .submit_prompt("same text")
        .unwrap_or_else(|error| panic!("prompt A: {error}"));
    client
        .pump()
        .unwrap_or_else(|error| panic!("pump A: {error}"));
    client
        .interrupt()
        .unwrap_or_else(|error| panic!("interrupt A: {error}"));
    client
        .pump()
        .unwrap_or_else(|error| panic!("interrupted pump: {error}"));
    assert_eq!(
        client.connection.user_inputs(&a),
        vec![Some("msg_only".to_owned())]
    );
    assert!(matches!(
        client.view().map(|view| &view.execution),
        Some(ExecutionState::Settled {
            outcome: Outcome::Interrupted,
            ..
        })
    ));
    let b = client
        .create_session()
        .unwrap_or_else(|error| panic!("create B: {error}"));
    assert!(
        client.submit_prompt("same text").is_err(),
        "an interruption must not free the input id for another session"
    );
    assert!(client.connection.user_inputs(&b).is_empty());
    assert_eq!(
        client.connection.user_inputs(&a),
        vec![Some("msg_only".to_owned())]
    );
}

/// A client that keeps at most two unconfirmed prompts, with one each in
/// sessions A and B (review SS-H3).
fn two_unconfirmed() -> (
    Client<ContractDouble>,
    crate::wire::SessionWireId,
    crate::wire::SessionWireId,
) {
    let bounds = Bounds {
        max_retained_sessions: 2,
        ..Bounds::PRODUCTION
    };
    let mut client = Client::new(ContractDouble::new(), bounds)
        .with_input_ids(std::sync::Arc::new(CountingIds::default()));
    if let Err(error) = client.select_first_offer() {
        panic!("the double must offer a selection: {error}");
    }
    let mut unconfirmed = || {
        let Ok(session) = client.create_session() else {
            panic!("create");
        };
        client
            .connection
            .set_fault(Fault::PromptLostBeforeAdmission);
        assert_eq!(
            client.submit_prompt("kept unresolved draft"),
            Err(ClientError::Unconfirmed)
        );
        session
    };
    let (a, b) = (unconfirmed(), unconfirmed());
    (client, a, b)
}

#[test]
fn a_third_unconfirmed_prompt_is_refused_and_the_kept_retries_still_run_once() {
    let (mut client, a, b) = two_unconfirmed();
    let Ok(c) = client.create_session() else {
        panic!("create C");
    };
    assert_eq!(
        client.submit_prompt("kept unresolved draft"),
        Err(ClientError::UnconfirmedElsewhere { held: 2 })
    );
    assert_eq!(client.pending.len(), 2);
    assert!(client.connection.user_inputs(&c).is_empty(), "C was sent");
    for (session, id) in [(&a, "msg_witness0"), (&b, "msg_witness1")] {
        let Ok(()) = client.select(session) else {
            panic!("select");
        };
        assert_eq!(client.submit_prompt("kept unresolved draft"), Ok(()));
        assert_eq!(
            client.connection.user_inputs(session),
            vec![Some(id.to_owned())],
            "an older retry id was lost at capacity"
        );
    }
    assert!(client.pending.is_empty());
}

#[test]
fn at_capacity_an_edit_does_not_replace_an_unresolved_prompt() {
    let (mut client, _, b) = two_unconfirmed();
    assert_eq!(
        client.submit_prompt("edited draft"),
        Err(ClientError::UnconfirmedEdit)
    );
    assert!(client.connection.user_inputs(&b).is_empty());
    assert_eq!(client.pending.len(), 2);
    assert_eq!(client.retry_prompt("edited draft"), Ok(false));
    assert_eq!(
        client.connection.user_inputs(&b),
        vec![Some("msg_witness1".to_owned())]
    );
    assert_eq!(client.pending.len(), 1);
}

#[test]
fn a_wire_refused_oversized_prompt_is_not_retained() {
    let (mut client, _) = submitting();
    let text = "x".repeat(100_000);
    assert!(matches!(
        client.submit_prompt(&text),
        Err(ClientError::Wire(_))
    ));
    assert!(client.pending.is_empty(), "the refused prompt was kept");
}

#[test]
fn a_refused_edit_keeps_the_unconfirmed_retry() {
    let (mut client, session) = submitting();
    client
        .connection
        .set_fault(Fault::PromptLostBeforeAdmission);
    assert_eq!(
        client.submit_prompt("do the task"),
        Err(ClientError::Unconfirmed)
    );
    assert!(client.submit_prompt(&"x".repeat(100_000)).is_err());
    assert_eq!(client.submit_prompt("do the task"), Ok(()));
    assert_eq!(
        client.connection.user_inputs(&session),
        vec![Some("msg_witness0".to_owned())],
        "the refused edit replaced the kept retry"
    );
}

mod uncertainty;

mod restart;

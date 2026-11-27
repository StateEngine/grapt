//! Recovery retains the controller and its original uncertain input identity.
use super::*;

#[test]
fn reconnect_never_replays_an_uncertain_prompt_and_keeps_explicit_retry_identity() {
    for fault in [
        Fault::PromptLostBeforeAdmission,
        Fault::PromptReplyLostUnconfirmed,
    ] {
        let (mut client, session) = submitting();
        client.connection.set_fault(fault);
        assert_eq!(
            client.submit_prompt("original"),
            Err(ClientError::Unconfirmed)
        );
        let admitted = client.connection.user_inputs(&session);
        client.connection.restart();
        for _ in 0..4 {
            assert!(client.pump().is_ok());
        }
        assert_eq!(client.selected(), Some(&session));
        assert_eq!(
            client.connection.user_inputs(&session),
            admitted,
            "reconnect replayed an input"
        );
        assert!(client.pending.contains_key(&session));
        assert_eq!(client.retry_prompt("edited draft"), Ok(false));
        assert_eq!(
            client.connection.user_inputs(&session),
            [Some("msg_witness0".to_owned())]
        );
        assert!(client.pending.is_empty());
        assert_eq!(client.submit_prompt("edited draft"), Ok(()));
        assert_eq!(
            client.connection.user_inputs(&session),
            [
                Some("msg_witness0".to_owned()),
                Some("msg_witness1".to_owned())
            ]
        );
    }
}

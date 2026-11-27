//! A prepared source claim cannot recreate work after Session deletion.
use super::*;

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn seat03_deletion_between_planning_and_claim_commit_preserves_source_work() -> TestResult {
    let (_root, application, _driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owners = native.snapshot()?;
    let owner = owners.first().ok_or("native owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?;
    let admission = async {
        let mut context = owner.context.lock().await;
        context.use_test_port(port);
        for _ in 0..16 {
            let live = context
                .converge()
                .await
                .map_err(|error| error.to_string())?;
            if live
                && context
                    .runtime
                    .as_ref()
                    .is_some_and(|runtime| !runtime.delivery_work_pending())
            {
                break;
            }
        }
        assert!(
            context.runtime.as_ref().is_some_and(HttpMailbox::is_live),
            "fake subscription never became live"
        );
        let binding = context.binding.as_ref().ok_or("binding")?;
        let capability = binding.capability.clone();
        let expectation = MailBindingExpectation::new(
            InstructionKey::new("stateengine-identity").ok_or("key")?,
            binding.raw.clone(),
        )?;
        let claim = application
            .lock_inbox()
            .store()
            .prepare_remote_mail_wake(
                &session,
                &binding.scope,
                expectation,
                grapt_types::MailboxCursor::START,
            )?
            .claim
            .ok_or("verified wake claim")?;
        let input = PendingInput {
            input: "msg_deleted_native_wake".parse()?,
            kind: grapt_inbox::PromotedInputKind::Synthetic,
            text: "Read mail-a and mail-b through the authenticated inbox".into(),
            description: Some("Native inbox wake".into()),
            metadata: Some(claim.metadata()),
            delivery: DeliveryMode::Queue,
            seq: 0,
            created_ms: application.clock.now().as_millis(),
        };
        application.get_record(&session)?; // The earlier planning read succeeds.
        {
            let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
            grapt_session::delete_session(
                application.lock_inbox().store_mut(),
                &authorities,
                &session,
            )?;
        }
        assert!(
            application.get_record(&session).is_err(),
            "Session deletion did not commit"
        );
        assert!(
            NativeMailOwners::commit_wake(&application, &context, &capability, &claim, &input)
                .is_err(),
            "deleted Session accepted a new native input"
        );
        assert!(
            application
                .lock_inbox()
                .pending_inputs(&session)?
                .is_empty(),
            "wake admission resurrected deleted Session input"
        );
        let remaining = application.lock_inbox().store().read_remote_mailbox(
            &binding.scope,
            grapt_types::MailboxCursor::START,
            64,
            262144,
        )?;
        assert_eq!(
            remaining
                .records
                .iter()
                .filter(|row| row.actionable())
                .count(),
            2,
            "refused deleted Session consumed source work"
        );
        context.finish().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (transport, witnessed) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        tokio::join!(fake::serve(listener, None, 2), admission)
    })
    .await?;
    transport?;
    witnessed?;
    Ok(())
}

use super::*;
use serde_json::json;
use tokio::{io::AsyncWriteExt, net::TcpListener};

const IDENTITY: &str = r#"{"scope":"project","project_id":"fixture-project","agent_id":"agt_fixture","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#;
const FLOOR: &str = "capability-floor-changed-retry";

async fn refuse(context: &mut NativeMailContext, reason: Option<&str>) -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    context.use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let serve = async {
        let (mut socket, _) = listener.accept().await?;
        let request = http::request(&mut socket, 2).await?;
        assert_eq!(request["operation"], "open");
        let mut failure = json!({"family":"OpenFailure","tag":"Unauthenticated","effect":"none"});
        if let Some(reason) = reason {
            failure["reason"] = json!(reason);
        }
        let body = serde_json::to_vec(&json!({"ok":false,"failure":failure}))?;
        socket.write_all(format!("HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
        socket.write_all(&body).await?;
        socket.shutdown().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let step = StepNumber::new(1).ok_or("step")?;
    let (served, refreshed) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(serve, context.refresh(step))
    })
    .await?;
    served?;
    assert_eq!(refreshed, Err(E::Transport));
    context.finish().await?;
    Ok(())
}

#[tokio::test]
async fn review_floor_refusal_allows_three_reopens_then_requires_rebind() -> TestResult {
    let mut context = context(Some(IDENTITY), true)?;
    for refusal in 1..=4 {
        refuse(&mut context, Some(FLOOR)).await?;
        assert_eq!(
            context.requires_rebind(),
            refusal == 4,
            "wrong retry allowance at refusal {refusal}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn review_other_authentication_reasons_require_rebind_immediately() -> TestResult {
    for reason in [
        None,
        Some("capability-required"),
        Some("grant-revoked"),
        Some("capability-floor-changed-retry-extra"),
    ] {
        let mut context = context(Some(IDENTITY), true)?;
        refuse(&mut context, reason).await?;
        assert!(context.requires_rebind(), "permanent refusal was retried");
    }
    Ok(())
}

#[tokio::test]
async fn review_authenticated_rebind_restores_the_floor_retry_allowance() -> TestResult {
    for refusals in [3, 4] {
        let mut context = context(Some(IDENTITY), true)?;
        for _ in 0..refusals {
            refuse(&mut context, Some(FLOOR)).await?;
        }
        assert_eq!(context.requires_rebind(), refusals == 4);
        context
            .custody
            .lock()
            .map_err(|_| "custody")?
            .put(context.session.clone(), "fixture-secret")?;
        assert!(
            !context.requires_rebind(),
            "authenticated replacement stayed refused"
        );
        refuse(&mut context, Some(FLOOR)).await?;
        assert!(
            !context.requires_rebind(),
            "old retry debt crossed the authenticated rebind"
        );
    }
    Ok(())
}

fn identity(context: &NativeMailContext, raw: &str) -> TestResult {
    let mut inbox = context.inbox.lock().map_err(|_| "inbox")?;
    InstructionService::put_entry(
        inbox.store_mut(),
        &context.session,
        &InstructionKey::new("stateengine-identity").ok_or("key")?,
        raw,
        2,
    )?;
    Ok(())
}

#[tokio::test]
async fn review_disabled_mail_preserves_retry_generation_until_authenticated_replacement(
) -> TestResult {
    for replacement in [false, true] {
        let mut context = context(Some(IDENTITY), true)?;
        for _ in 0..3 {
            refuse(&mut context, Some(FLOOR)).await?;
        }
        assert!(!context.requires_rebind());
        identity(&context, r#"{"scope":"incognito"}"#)?;
        context
            .refresh(StepNumber::new(1).ok_or("step")?)
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(
            context
                .attention_snapshot(false)
                .map_err(|e| e.to_string())?,
            MailAttentionInfo::Disabled
        );
        assert!(context.take_batch().map_err(|e| e.to_string())?.is_none());
        identity(&context, IDENTITY)?;
        if replacement {
            context
                .custody
                .lock()
                .map_err(|_| "custody")?
                .put(context.session.clone(), "fixture-secret")?;
        }
        refuse(&mut context, Some(FLOOR)).await?;
        assert_eq!(
            context.requires_rebind(),
            !replacement,
            "disabled mail lost its capability generation; replacement={replacement}"
        );
    }
    Ok(())
}

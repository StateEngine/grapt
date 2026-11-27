use super::*;
use crate::dogfood::{context::SessionCapabilities, native_mail::NativeMailContext};
use grapt_types::{ClockAuthority, IdAuthority, IdToken, InstructionKey, Timestamp};
use std::sync::Mutex;

#[derive(Debug, Default)]
struct Authorities(AtomicUsize);

impl ClockAuthority for Authorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_780_000_000_000)
    }
}

impl IdAuthority for Authorities {
    fn mint(&self) -> IdToken {
        IdToken::new(format!(
            "mail{:012x}",
            self.0.fetch_add(1, Ordering::SeqCst)
        ))
        .unwrap_or_else(|| panic!("fixture identity"))
    }
}

fn mail(binding: &str) -> Result<crate::dogfood::native_mail::custody::NativeMailPlan, String> {
    let authorities = Arc::new(Authorities::default());
    let mut store = grapt_store::EventStore::open_in_memory().map_err(|e| e.to_string())?;
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_mail_outage").ok_or("project")?,
        "/fixture",
        "mail outage",
    )
    .map_err(|e| e.to_string())?;
    let session = grapt_session::create_session(
        &mut store,
        &grapt_types::DeterministicAuthorities::new(&*authorities, &*authorities),
        &seed,
        &mut [],
    )
    .map_err(|e| e.to_string())?
    .session;
    grapt_instruction::InstructionService::ensure_tables(&mut store).map_err(|e| e.to_string())?;
    grapt_instruction::InstructionService::put_entry(
        &mut store,
        &session,
        &InstructionKey::new("stateengine-identity").ok_or("key")?,
        binding,
        1,
    )
    .map_err(|e| e.to_string())?;
    let context = NativeMailContext::new(
        session,
        Arc::new(Mutex::new(
            grapt_inbox::Inbox::new(store).map_err(|e| e.to_string())?,
        )),
        Arc::new(Mutex::new(SessionCapabilities::default())),
        authorities.clone(),
        authorities,
    )
    .map_err(|e| e.to_string())?;
    Ok(crate::dogfood::native_mail::custody::NativeMailPlan::private(context))
}

#[tokio::test]
async fn missing_mail_capability_allows_requests_after_each_cleanup() -> Result<(), String> {
    for family in [
        ProtocolFamily::ChatGptCodex,
        ProtocolFamily::OpenAiResponses,
    ] {
        let step = StepNumber::new(1).ok_or("step")?;
        let (continuation, _scratch) = DurableContinuation::empty_for_test("mail-outage")?;
        let mut plan = ProductPlan::new(
            family,
            "hello".into(),
            cache_key("mail_outage")?,
            None,
            targets(family)?,
            Arc::new(crate::dogfood::mcp::PublishedTools::default()),
            continuation,
            step,
        );
        plan.native_mail = Some(mail(
            r#"{"project_id":"fixture","agent_id":"fixture-seat"}"#,
        )?);
        for ordinal in 1..=2 {
            plan.refresh_context(step, HistoryGeneration::new(1))
                .await
                .map_err(|e| format!("mail outage refused preparation: {e:?}"))?;
            let prepared = plan
                .plan(step, ordinal, HistoryGeneration::new(1))
                .map_err(|e| e.detail)?;
            let input = match &prepared.request {
                ProviderRequest::ChatGptCodex(request) => &request.input,
                ProviderRequest::OpenAiResponses(request) => &request.input,
                _ => return Err("unexpected family".into()),
            };
            assert_eq!(
                input
                    .iter()
                    .filter(|item| matches!(item,
                grapt_provider::CodexInputItem::Message(message)
                    if message.content.starts_with("StateEngine mail is unavailable this turn:")))
                    .count(),
                1
            );
            assert!(prepared.mail.is_none());
            plan.finish_context(false).await?;
        }
        plan.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn malformed_mail_identity_still_refuses_preparation() -> Result<(), String> {
    let family = ProtocolFamily::ChatGptCodex;
    let step = StepNumber::new(1).ok_or("step")?;
    let (continuation, _scratch) = DurableContinuation::empty_for_test("mail-identity")?;
    let mut plan = ProductPlan::new(
        family,
        "hello".into(),
        cache_key("mail_identity")?,
        None,
        targets(family)?,
        Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        continuation,
        step,
    );
    plan.native_mail = Some(mail(r#"{"project_id":"fixture"}"#)?);
    assert_eq!(
        plan.refresh_context(step, HistoryGeneration::new(1)).await,
        Err(
            grapt_runtime::provider_source::ContextPreparationFailure::Continuity(
                grapt_session::execution::ContinuityFailure::Binding
            )
        )
    );
    plan.finish_context(true).await?;
    plan.shutdown().await?;
    Ok(())
}

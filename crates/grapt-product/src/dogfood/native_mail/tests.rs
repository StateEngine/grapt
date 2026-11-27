use super::*;
use grapt_mail_transport::identity::ExclusiveSourceCursor;
use grapt_types::IdToken;
pub(in crate::dogfood::native_mail) mod http;
mod origin;
mod retry_floor;
#[derive(Debug)]
struct Ids;
impl IdAuthority for Ids {
    fn mint(&self) -> IdToken {
        IdToken::new("0000000000000001").unwrap_or_else(|| panic!("id"))
    }
}
type TestResult = Result<(), Box<dyn std::error::Error>>;
pub(in crate::dogfood::native_mail) fn context(
    binding: Option<&str>,
    capability: bool,
) -> Result<NativeMailContext, Box<dyn std::error::Error>> {
    context_at(binding, capability, "/fixture")
}
pub(in crate::dogfood::native_mail) fn context_at(
    binding: Option<&str>,
    capability: bool,
    directory: &str,
) -> Result<NativeMailContext, Box<dyn std::error::Error>> {
    let mut store = grapt_store::EventStore::open_in_memory()?;
    let clock = super::custody::tests::Clock;
    let authorities = grapt_types::DeterministicAuthorities::new(&clock, &Ids);
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_native_fixture").ok_or("project")?,
        directory,
        "native binding",
    )?;
    let session = grapt_session::create_session(&mut store, &authorities, &seed, &mut [])?.session;
    InstructionService::ensure_tables(&mut store)?;
    if let Some(raw) = binding {
        InstructionService::put_entry(
            &mut store,
            &session,
            &InstructionKey::new("stateengine-identity").ok_or("key")?,
            raw,
            1,
        )?;
    }
    let inbox = Arc::new(Mutex::new(Inbox::new(store)?));
    let custody = Arc::new(Mutex::new(SessionCapabilities::default()));
    if capability {
        custody
            .lock()
            .map_err(|_| "custody")?
            .put(session.clone(), "fixture-secret")?;
    }
    Ok(NativeMailContext::new(
        session,
        inbox,
        custody,
        Arc::new(Ids),
        Arc::new(super::custody::tests::Clock),
    )
    .map_err(|e| e.to_string())?)
}
#[tokio::test]
async fn absent_and_incognito_identity_disable_mail_without_open_or_consumption() -> TestResult {
    for binding in [None, Some(r#"{"scope":"incognito"}"#)] {
        let mut context = context(binding, false)?;
        context
            .refresh(StepNumber::new(1).ok_or("step")?)
            .await
            .map_err(|e| e.to_string())?;
        assert!(context.take_batch().map_err(|e| e.to_string())?.is_none());
        assert!(!context.pending);
        assert!(context.runtime.is_none());
        context.finish().await?;
    }
    Ok(())
}
#[test]
fn invalid_or_uncustodied_binding_is_a_refusal_and_replacement_revokes_prepared_batch() -> TestResult
{
    let bound = r#"{"project_id":"secode","agent_id":"coder"}"#;
    let missing = context(Some(bound), false)?;
    assert!(matches!(missing.current_binding(), Err(E::Capability)));
    let invalid = context(Some(r#"{"project_id":"secode"}"#), true)?;
    assert!(matches!(invalid.current_binding(), Err(E::Binding)));
    let mut context = context(Some(bound), true)?;
    context.binding = context.current_binding().map_err(|e| e.to_string())?;
    context.prepared = true;
    context
        .custody
        .lock()
        .map_err(|_| "custody")?
        .put(context.session.clone(), "fixture-secret")?;
    assert!(
        matches!(context.take_batch(), Err(E::Rebound)),
        "replacement with identical bytes must revoke old custody"
    );
    assert!(matches!(context.take_batch(), Err(E::Incomplete)));
    Ok(())
}
#[test]
fn durable_binding_change_cannot_reuse_old_native_mail_scope() -> TestResult {
    let mut context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    context.binding = context.current_binding().map_err(|e| e.to_string())?;
    context.prepared = true;
    let mut inbox = context.inbox.lock().map_err(|_| "inbox")?;
    InstructionService::put_entry(
        inbox.store_mut(),
        &context.session,
        &InstructionKey::new("stateengine-identity").ok_or("key")?,
        r#"{"project_id":"secode","agent_id":"foreign"}"#,
        2,
    )?;
    drop(inbox);
    assert!(matches!(context.take_batch(), Err(E::Rebound)));
    Ok(())
}

#[test]
fn changing_only_binding_metadata_revokes_the_prepared_batch() -> TestResult {
    let mut context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    context.binding = context.current_binding().map_err(|e| e.to_string())?;
    context.prepared = true;
    let mut inbox = context.inbox.lock().map_err(|_| "inbox")?;
    InstructionService::put_entry(
        inbox.store_mut(),
        &context.session,
        &InstructionKey::new("stateengine-identity").ok_or("key")?,
        r#"{"project_id":"secode","agent_id":"coder","scope":"project"}"#,
        2,
    )?;
    drop(inbox);
    assert!(matches!(context.take_batch(), Err(E::Rebound)));
    Ok(())
}

#[test]
fn unreadable_instruction_owner_is_not_an_absent_binding() -> TestResult {
    let context = context(None, false)?;
    context
        .inbox
        .lock()
        .map_err(|_| "inbox")?
        .store()
        .connection()
        .execute("DROP TABLE instruction_entry", [])?;
    assert!(matches!(context.current_binding(), Err(E::Binding)));
    Ok(())
}

#[test]
fn nmv2_native_protocol_is_explicit_and_unknown_protocol_refuses() -> TestResult {
    for (raw, expected) in [
        (
            r#"{"project_id":"secode","agent_id":"coder"}"#,
            progress::MailTransportVersion::V1,
        ),
        (
            r#"{"project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#,
            progress::MailTransportVersion::V2,
        ),
    ] {
        assert_eq!(
            context(Some(raw), true)?
                .current_binding()
                .map_err(|e| e.to_string())?
                .ok_or("binding")?
                .protocol,
            expected
        );
    }
    let invalid = context(
        Some(r#"{"project_id":"secode","agent_id":"coder","mail_transport_protocol":"future"}"#),
        true,
    )?;
    assert!(matches!(invalid.current_binding(), Err(E::Binding)));
    Ok(())
}

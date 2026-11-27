//! Application boundary witness; network/retained witnesses remain separate.
use super::*;
use crate::dogfood::{elicitation::FormProducer, mcp::channel::ServerRequestHandler};
use grapt_types::*;
use std::{path::PathBuf, sync::Arc, time::Duration};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn lookup_error(error: FormLookupApplicationError) -> String {
    match error {
        FormLookupApplicationError::InvalidRequest(detail) => {
            format!("lookup invalid: {:?}", serde_json::to_value(detail))
        }
        other => format!("lookup refusal: {other:?}"),
    }
}

fn target() -> Result<ResolvedModelTarget, String> {
    let provider = ProviderId::new("fixture").ok_or("provider")?;
    let model = ModelId::new("fixture").ok_or("model")?;
    let catalog = ModelCatalog::load(
        ModelCatalogVersion::new("fixture").ok_or("catalog version")?,
        vec![ModelCatalogRecord::new(
            provider.clone(),
            model.clone(),
            BaseModelId::new("fixture").ok_or("base")?,
            ModelFamilyId::new("fixture").ok_or("family")?,
            None,
        )],
    )
    .ok_or("catalog")?;
    ModelRegistry::install(catalog)
        .issue(
            &provider,
            &model,
            None,
            RoutingRequest {
                route: ModelRouteId::new("fixture").ok_or("route")?,
                account: AccountId::new("fixture").ok_or("account")?,
                credential_version: CredentialVersion::new("fixture")
                    .ok_or("credential version")?,
                policy: PolicyVersion::new("fixture").ok_or("policy")?,
            },
        )
        .map_err(|error| format!("target: {error:?}"))
}

struct Root(PathBuf);
impl Root {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let ids = grapt_store::OsRandomIds::seed()?;
        let root = std::env::temp_dir().join(format!("global-form-api-{}", ids.mint().as_str()));
        std::fs::create_dir(&root)?;
        let root = Self(root);
        let initialized = std::process::Command::new("git")
            .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
            .args(["init", "--quiet"])
            .arg(&root.0)
            .status()?;
        if !initialized.success() {
            return Err("global form fixture Git boundary could not be initialized".into());
        }
        Ok(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        // This fresh minted fixture root owns its Git metadata and artifacts.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn application_forms_validate_location_and_resume_the_originating_request() -> TestResult {
    let root = Root::new()?;
    let other = Root::new()?;
    let hub = crate::dogfood::event_hub()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        Arc::clone(&hub),
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let producer = FormProducer {
        hooks: None,
        inbox: Arc::clone(&app.inbox),
        ids: Arc::clone(&app.ids),
        clock: Arc::clone(&app.clock),
        hub,
    };
    let directory = root.0.to_str().ok_or("fixture path encoding")?;
    let query: ProjectCurrentLocationQueryDto =
        decode(json!({"location":{"directory":directory}}))?;
    let wrong: ProjectCurrentLocationQueryDto =
        decode(json!({"location":{"directory":other.0.to_str().ok_or("other path encoding")?}}))?;
    for cancelled in [false, true] {
        let mut live = producer.connect(directory, "greeter")?;
        let request = grapt_mcp::incoming::server_request(grapt_mcp::McpBounds::conservative()?,
            br#"{"jsonrpc":"2.0","id":"name","method":"elicitation/create","params":{"message":"Name?","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}}}"#)?
            .ok_or("server request")?;
        let mut wait = live.answer(&request, None);
        assert!(tokio::time::timeout(Duration::from_millis(1), &mut wait)
            .await
            .is_err());
        let pending = grapt_form::global_catalog::pending_global_forms(app.lock_inbox().store())?;
        assert_eq!(pending.len(), 1);
        let id = pending[0].id.clone();
        let info = get(app.as_ref().clone(), id.clone(), query.clone())
            .await
            .map_err(lookup_error)?;
        assert_eq!(serde_json::to_value(info)?["data"]["sessionID"], "global");
        assert!(matches!(
            get(app.as_ref().clone(), id.clone(), wrong.clone()).await,
            Err(FormLookupApplicationError::FormNotFound(_))
        ));
        let invalid_answer = decode(json!({"answer":{"name":42}}))?;
        assert!(matches!(
            reply(
                app.as_ref().clone(),
                id.clone(),
                query.clone(),
                invalid_answer
            )
            .await,
            Err(FormReplyApplicationError::FormInvalidAnswer(_))
        ));
        if cancelled {
            cancel(app.as_ref().clone(), id.clone(), query.clone())
                .await
                .map_err(|error| format!("application refusal: {error:?}"))?;
        } else {
            reply(
                app.as_ref().clone(),
                id.clone(),
                query.clone(),
                decode(json!({"answer":{"name":"Ada"}}))?,
            )
            .await
            .map_err(|error| format!("reply refusal: {error:?}"))?;
        }
        let answer = tokio::time::timeout(Duration::from_secs(1), &mut wait).await??;
        assert_eq!(
            answer["action"],
            if cancelled { "cancel" } else { "accept" }
        );
        if !cancelled {
            assert_eq!(answer["content"]["name"], "Ada");
        }
        assert!(matches!(
            cancel(app.as_ref().clone(), id.clone(), query.clone()).await,
            Err(FormCancelApplicationError::FormAlreadySettled(_))
        ));
        let state = state(app.as_ref().clone(), id, query.clone())
            .await
            .map_err(lookup_error)?;
        assert_eq!(
            serde_json::to_value(state)?["data"]["status"],
            if cancelled { "cancelled" } else { "answered" }
        );
    }
    Ok(())
}

/// The dispatch reaches the DURABLE event, and it is absent when no call frames
/// the request.
///
/// The binding type's own tests prove the shape and the compatibility rule.
/// This proves the CARRY: that the identity actually travels from the handler
/// argument into the event the store keeps, which is the whole point of the
/// change and the one thing a type-level test cannot show.
#[tokio::test]
async fn a_framing_call_reaches_the_durable_form_event_and_its_absence_is_absent() -> TestResult {
    use grapt_form::global::CallDispatch;
    let root = Root::new()?;
    let hub = crate::dogfood::event_hub()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        Arc::clone(&hub),
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let producer = FormProducer {
        hooks: None,
        inbox: Arc::clone(&app.inbox),
        ids: Arc::clone(&app.ids),
        clock: Arc::clone(&app.clock),
        hub,
    };
    let directory = root.0.to_str().ok_or("fixture path encoding")?;

    for dispatch in [
        Some(CallDispatch {
            call: grapt_types::ToolCallId::new("call_carried").ok_or("call")?,
            generation: 3,
            attempt: Some("pat_carried".to_owned()),
        }),
        None,
    ] {
        let mut live = producer.connect(directory, "greeter")?;
        let request = grapt_mcp::incoming::server_request(grapt_mcp::McpBounds::conservative()?,
            br#"{"jsonrpc":"2.0","id":"name","method":"elicitation/create","params":{"message":"Name?","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}}}"#)?
            .ok_or("server request")?;
        let mut wait = live.answer(&request, dispatch.as_ref());
        assert!(tokio::time::timeout(Duration::from_millis(1), &mut wait)
            .await
            .is_err());
        let pending = grapt_form::global_catalog::pending_global_forms(app.lock_inbox().store())?;
        let record = pending.last().ok_or("no pending form")?;
        assert_eq!(
            record.binding.call, dispatch,
            "the dispatch the handler was given is the one the store holds"
        );
        // OWNERSHIP IS UNCHANGED, and that is asserted here rather than assumed:
        // the form still declares the literal global session and is still owned
        // by its elicitation, dispatch or no dispatch.
        let raw = app.lock_inbox().store().connection().query_row(
            "SELECT data FROM event WHERE aggregate_family='form' AND aggregate_id=?1
             AND type='form.created.2' ORDER BY seq LIMIT 1",
            [record.id.as_str()],
            |row| row.get::<_, String>(0),
        )?;
        let data: serde_json::Value = serde_json::from_str(&raw)?;
        assert_eq!(data["form"]["sessionID"], "global");
        assert_eq!(
            data["owner"].get("dispatch").is_some(),
            dispatch.is_some(),
            "an absent dispatch must be ABSENT from the payload, never null"
        );
        drop(wait);
    }
    Ok(())
}

#[tokio::test]
async fn auth_composition_preserves_provider_startup_with_a_fresh_profile() -> TestResult {
    let root = Root::new()?;
    let profile = root.0.join("fresh-profile");
    assert!(!profile.exists());
    let (app, driver) = crate::dogfood::test_provider::open(
        &profile,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    assert!(profile.join("dogfood-artifacts").is_dir());
    assert!(app.mcp.auth.is_some());
    drop(driver);
    drop(app);
    Ok(())
}

#[tokio::test]
async fn startup_form_list_and_lookup_preserve_the_same_reply_guard_input() -> TestResult {
    let root = Root::new()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let directory = root.0.to_str().ok_or("fixture path encoding")?;
    let query: ProjectCurrentLocationQueryDto =
        decode(json!({"location":{"directory":directory}}))?;
    let id: FormId = "frm_startup_projection".parse()?;
    let binding = grapt_form::global::GlobalFormBinding {
        elicitation: McpElicitationId::new("startup-projection").ok_or("elicitation")?,
        connection: McpElicitationId::new("startup-connection").ok_or("connection")?,
        location: LocationRef::new(directory, None)?,
        call: None,
    };
    let params = json!({"mode":"url","message":"Connect manually",
        "url":"https://example.invalid/connect","elicitationId":"external-startup"});
    let url = grapt_mcp::elicitation::UrlElicitation::parse(&params)?;
    let info = json!({"id":id.as_str(),"sessionID":"global","title":"Startup consent",
        "metadata":{"kind":"mcp-url-elicitation","server":"consent","startup":true,
            "message":url.message(),"url":url.url(),"host":url.host(),
            "elicitationId":url.id().as_str()},"fields":url.fields()});
    app.lock_inbox()
        .store_mut()
        .elect_writer(&grapt_store::AggregateId::Form(id.clone()))?;
    let event = app.mint_event()?;
    FormService::create_global_on(
        app.lock_inbox().store_mut(),
        &event,
        &id,
        &binding,
        &info,
        app.clock.now().as_millis(),
    )?;
    for suspended in [false, true] {
        if suspended {
            let event = app.mint_event()?;
            FormService::suspend_on(
                app.lock_inbox().store_mut(),
                &event,
                &id,
                FormSuspensionReason::Shutdown,
                app.clock.now().as_millis(),
            )?;
        }
        let listed = crate::dogfood::form_catalog::list(app.as_ref().clone(), query.clone())
            .await
            .map_err(|error| format!("list refusal: {error:?}"))?;
        let listed = serde_json::to_value(listed)?;
        let fetched = get(app.as_ref().clone(), id.clone(), query.clone())
            .await
            .map_err(lookup_error)?;
        let fetched = serde_json::to_value(fetched)?;
        assert_eq!(listed["data"][0]["metadata"]["startupPending"], !suspended);
        assert_eq!(fetched["data"], listed["data"][0]);
    }
    Ok(())
}

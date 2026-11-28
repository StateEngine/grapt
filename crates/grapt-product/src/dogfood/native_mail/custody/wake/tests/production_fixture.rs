//! The normal execution, promotion and provider owners with synthetic inputs.
use super::*;
use serde_json::{json, Value};
use std::path::Path;

pub(super) async fn open(
    initial: &Value,
    packet: &Path,
) -> Result<
    (
        Root,
        Arc<ServerApplication>,
        crate::dogfood::prompt_driver::PromptDriver,
        SessionId,
    ),
    Box<dyn std::error::Error>,
> {
    let root = Root::create(
        Path::new(
            initial["service_root"]
                .as_str()
                .ok_or("synthetic service root")?,
        )
        .to_path_buf(),
    )?;
    std::fs::write(root.0.join("service.conf"), "slot=product\ninstance=mail-consumer\nenvironment=local\nversion=0.1.0\nserve_window_ms=60000\nprovider_mode=production\nprovider_connect_ms=3000\nprovider_response_head_ms=3000\nprovider_active_attempts=1\n")?;
    let config = json!({"store":{"authority":"initialize"},"product":{"build":"native-mail-test"},
        "provider":{"id":"openai","model":"fixture-model","base_model":"fixture-model","family":"openai-responses","route":"fixture-route","account":"fixture-account","credential_version":"fixture-v1","policy_version":"fixture-policy","variants":[]},
        "catalog":{"source":"models.dev-chatgpt"},"compaction":{},
        "mcp":{"stateengine":{"type":"local","command":["python3",initial["mcp_script"],packet],"codemode":false}}
    });
    std::fs::write(root.0.join("grapt.jsonc"), serde_json::to_vec(&config)?)?;
    std::fs::write(
        root.0.join("provider.credential"),
        "synthetic-loopback-provider-secret",
    )?;
    std::fs::write(root.0.join("basic.credential"), "fixture:secret")?;
    std::fs::write(
        root.0.join("model-catalog.json"),
        serde_json::to_vec(
            &json!({"openai":{"id":"openai","name":"synthetic provider","npm":"@ai-sdk/openai","models":{"fixture-model":{"id":"fixture-model","name":"synthetic model","family":"gpt-5","tool_call":true,"modalities":{"input":["text"],"output":["text"]},"limit":{"output":32000}}}}}),
        )?,
    )?;
    std::fs::write(
        root.0.join("account-models.json"),
        serde_json::to_vec(
            &json!({"models":[{"slug":"fixture-model","visibility":"list","context_window":128000,"supported_reasoning_levels":[]}]}),
        )?,
    )?;
    let mut platform = grapt_platform::PlatformOwner::prepare(&root.0)?.into_provider_parts()?;
    let secondary_credentials = platform.take_secondary_credentials();
    let (store, targets, _build, credential, _contact) = platform.into_components();
    let model_port = u16::try_from(initial["model_port"].as_u64().ok_or("model fixture port")?)?;
    let endpoint = crate::dogfood::LiveEndpoint::Loopback(grapt_provider::LoopbackBase::parse(
        &format!("http://127.0.0.1:{model_port}"),
    )?);
    let hub = crate::dogfood::event_hub()?;
    let plugins = crate::dogfood::plugins::PluginRuntime::compose(&root.0)?;
    let (application, driver) = ServerApplication::open_production(
        &root.0,
        store,
        targets,
        credential,
        secondary_credentials,
        endpoint,
        crate::dogfood::production::ProductionProviderLimits {
            connect: std::time::Duration::from_secs(3),
            response_head: std::time::Duration::from_secs(3),
            active_attempts: std::num::NonZeroUsize::new(1).ok_or("capacity")?,
            child_attempts: std::num::NonZeroUsize::new(1).ok_or("child capacity")?,
        },
        hub,
        plugins.clone(),
    )?;
    plugins.bind(&application)?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let agent = crate::dogfood::selection::PRODUCTION_AGENT_IDS
        .first()
        .ok_or("composed agent")?;
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_normal_mail_consumer").ok_or("project")?,
        root.0.to_str().ok_or("directory")?,
        "retained mail execution",
    )?
    .with_agent(*agent)?;
    let session = grapt_runtime::SessionPorts::new(application.inbox.clone(), &authorities)
        .create(&seed)?
        .session;
    let identity=json!({"scope":"project","project_id":initial["project"],"agent_id":initial["recipient"],"mail_transport_protocol":"remote-seat-mail-transport/v2"}).to_string();
    InstructionService::put_entry(
        application.lock_inbox().store_mut(),
        &session,
        &InstructionKey::new("stateengine-identity").ok_or("key")?,
        &identity,
        1,
    )?;
    let response = ingress::call(
        &application,
        "PUT",
        &format!("/api/session/{session}/stateengine/capability"),
        &json!({"capability":initial["capability"]}).to_string(),
    )
    .await?;
    if !response.starts_with(b"HTTP/1.1 204") {
        return Err("normal consumer synthetic capability refused".into());
    }
    Ok((root, application, driver, session))
}

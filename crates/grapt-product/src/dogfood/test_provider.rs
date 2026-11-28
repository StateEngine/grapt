//! Application fixtures use the ordinary provider transport and execution owner.
use super::*;
use std::time::Duration;
#[path = "../../tests/support/provider_profile.rs"]
mod provider_profile;
#[path = "../../tests/support/scripted_provider.rs"]
mod scripted_provider;
pub(super) struct Server {
    _transport: scripted_provider::Server,
    observed: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    hold: Arc<std::sync::atomic::AtomicBool>,
}

impl Server {
    fn start() -> Result<(Self, std::net::SocketAddr), String> {
        let observed = Arc::new(std::sync::Mutex::new(None));
        let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let calls = observed.clone();
        let held = hold.clone();
        let transport = scripted_provider::Server::start(Vec::new(), move |body| {
            match calls.lock() {
                Ok(mut last) => *last = Some(body.clone()),
                Err(_) => panic!("provider observation mutex poisoned"),
            }
            held.load(std::sync::atomic::Ordering::Acquire)
        })?;
        let address = transport.address;
        Ok((
            Self {
                _transport: transport,
                observed,
                hold,
            },
            address,
        ))
    }

    pub(super) fn request(&self) -> Result<Option<serde_json::Value>, String> {
        self.observed
            .lock()
            .map(|last| last.clone())
            .map_err(|_| "provider observation mutex poisoned".into())
    }

    pub(super) fn hold_response(&self) {
        self.hold.store(true, std::sync::atomic::Ordering::Release);
    }
}

pub(super) fn server(app: &ServerApplication) -> Result<&Server, String> {
    app._test_provider
        .get()
        .ok_or_else(|| "application fixture provider absent".into())
}

pub(super) fn open(
    root: &Path,
    store: grapt_store::EventStore,
    target: &ResolvedModelTarget,
    hub: Arc<DogfoodEventHub>,
    plugins: Arc<plugins::PluginRuntime>,
) -> Result<(Arc<ServerApplication>, prompt_driver::PromptDriver), ServerRefusal> {
    let failure = |error: String| ServerRefusal::Compose(error);
    let plugins = if plugins.service_root().as_os_str().is_empty() {
        plugins::PluginRuntime::compose(root).map_err(failure)?
    } else {
        plugins
    };
    let binding = plugins.clone();
    let (server, address) = Server::start().map_err(failure)?;
    let ids = grapt_store::OsRandomIds::seed().map_err(|error| failure(error.to_string()))?;
    let inputs = root.join(format!("fixture-provider-{}", ids.mint().as_str()));
    std::fs::create_dir_all(root).map_err(|error| failure(error.to_string()))?;
    std::fs::create_dir(&inputs).map_err(|error| failure(error.to_string()))?;
    ensure_service_configuration(root).map_err(|error| failure(error.to_string()))?;
    let config = json!({"store":{"authority":"open-ready"},"product":{"build":"fixture"},
        "provider":{"id":target.provider().as_str(),"model":target.model().as_str(),
            "base_model":target.model().as_str(),"family":"openai-responses", "protocol_family":"openai-responses",
            "route":target.route().as_str(),"account":target.account().as_str(),
            "credential_version":target.credential_version().as_str(),"policy_version":"fixture-policy","variants":[]}});
    let catalog = json!({target.provider().as_str():{"id":target.provider().as_str(),"name":"Fixture provider", "npm":"@ai-sdk/openai", "models":{
        target.model().as_str():{"id":target.model().as_str(),"name":"Fixture model","family":"openai-responses", "tool_call":true,
            "modalities":{"input":["text"],"output":["text"]},"limit":{"context":128000,"output":32000}}}}});
    let listing = json!({"data":[{"id":target.model().as_str(),"max_model_len":128000}]});
    for (name, bytes) in [
        ("grapt.jsonc", serde_json::to_vec(&config)),
        ("model-catalog.json", serde_json::to_vec(&catalog)),
        ("account-models.json", serde_json::to_vec(&listing)),
    ] {
        std::fs::write(
            inputs.join(name),
            bytes.map_err(|error| failure(error.to_string()))?,
        )
        .map_err(|error| failure(error.to_string()))?;
    }
    std::fs::write(
        inputs.join("provider.credential"),
        "synthetic-loopback-provider-secret",
    )
    .map_err(|error| failure(error.to_string()))?;
    provider_profile::configure(&inputs, address).map_err(|error| failure(error.to_string()))?;
    let parts = provider_inputs(&inputs).map_err(failure)?;
    let capacity = std::num::NonZeroUsize::new(8).ok_or_else(|| failure("capacity".into()))?;
    let (app, driver) = ServerApplication::open_production(
        root,
        store,
        parts.targets,
        parts.credential,
        parts.secondary_credentials,
        LiveEndpoint::Production(grapt_provider::EndpointConfig::new()),
        ProductionProviderLimits {
            connect: Duration::from_secs(3),
            response_head: Duration::from_secs(3),
            active_attempts: capacity,
            child_attempts: capacity,
        },
        hub,
        plugins,
    )?;
    binding.bind(&app).map_err(failure)?;
    app._test_provider
        .set(server)
        .map_err(|_| failure("fixture server already attached".into()))?;
    Ok((app, driver))
}

fn ensure_service_configuration(root: &Path) -> std::io::Result<()> {
    use std::io::Write;
    match std::fs::OpenOptions::new().write(true).create_new(true).open(root.join("service.conf")) {
        Ok(mut file) => file.write_all(b"slot=product\ninstance=fixture\nenvironment=test\nversion=0.1.0\nserve_window_ms=1000\nprovider_mode=production\nprovider_connect_ms=3000\nprovider_response_head_ms=3000\nprovider_active_attempts=8\n"),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn provider_inputs(root: &Path) -> Result<grapt_platform::ProviderInputParts, String> {
    grapt_platform::PublishedConfig::load(root)
        .and_then(|config| config.ensure_connection_store())
        .map_err(|error| error.to_string())?;
    let store = grapt_connections::store::Store::open(root).map_err(|error| error.to_string())?;
    let records = store
        .load()
        .map_err(|error| error.to_string())?
        .ok_or("fixture connections absent")?;
    grapt_platform::PreparedProviderInputs::prepare_pending_connections(root, &records)
        .and_then(|inputs| inputs.into_components().map_err(Into::into))
        .map_err(|error| error.to_string())
}

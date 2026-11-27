//! Foreground volatile service, selected before any persistent product admission.
//!
//! Persistence omission.
//! Copyright 2025 OpenAI. Modified by Grapt for an in-memory service and
//! private readiness pipe; see licenses/openai-codex/.
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use grapt_api::{PreparedApiHost, TokioDeadlineDriver};
use grapt_store::{EventStore, OsRandomIds};
use grapt_types::{service::ProcessId, IdAuthority};

use super::{DogfoodComposition, ProductRefusal};
use crate::compose_host::{bind_loopback, ephemeral_service};
use crate::input::{read_config, Credential};

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn run(root: &Path) -> Result<(), ProductRefusal> {
    #[cfg(feature = "loopback-provider-testkit")]
    if std::env::var_os("GRAPT_NT_PATCH_INTERRUPTION") == Some("1".into()) {
        return Err(ProductRefusal::Input(
            "ephemeral runs do not support the disk-writing patch interruption fixture".into(),
        ));
    }
    let protection = grapt_login::profile_security::ProtectionBatch::begin()
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    // No PlatformOwner, store preflight, startup lock, installation guard,
    // registration, ready receipt, log or profile creation belongs on this path.
    let bootstrap = grapt_platform::PreparedProviderInputs::prepare(root)
        .map_err(|error| ProductRefusal::Input(error.to_string()))?;
    let grapt_platform::ProviderInputParts {
        targets,
        build,
        credential,
        secondary_credentials: secondary,
    } = bootstrap
        .into_components()
        .map_err(|error| ProductRefusal::Input(error.to_string()))?;
    let config = read_config(&root.join("service.conf"), build.as_str())?;
    let endpoint =
        crate::dogfood::production::provider_endpoint().map_err(ProductRefusal::Compose)?;
    let limits = super::endpoint::provider_limits(root, &config)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let mut store =
        EventStore::open_in_memory().map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    store
        .connection()
        .pragma_update(None, "temp_store", "MEMORY")
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    crate::commit_executor::install(&mut store);
    let hub = crate::dogfood::event_hub().map_err(ProductRefusal::Compose)?;
    let plugins = crate::dogfood::plugins::PluginRuntime::compose_ephemeral(root);
    let (application, driver) = crate::dogfood::DogfoodApplication::open_ephemeral(
        root,
        store,
        targets,
        credential,
        secondary,
        endpoint,
        limits,
        Arc::clone(&hub),
        Arc::clone(&plugins),
    )
    .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    plugins
        .bind(&application)
        .map_err(ProductRefusal::Compose)?;
    let mut driver = super::ServiceDriver::Ready(driver);
    let dogfood =
        DogfoodComposition::ready(application.clone(), application, Arc::clone(&hub), None);
    let ids = OsRandomIds::seed().map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let credential = Credential {
        username: "ephemeral".into(),
        secret: ids.mint().as_str().to_owned(),
    };
    let pid = ProcessId::new(std::process::id())
        .ok_or_else(|| ProductRefusal::Compose("process id is zero".into()))?;
    let control = super::terminal::Lifetime::terminal(false)?;
    protection
        .finish()
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    runtime.block_on(async {
        let listener = bind_loopback().await?;
        let host_limits = crate::compose_host::configured_host_limits(&config)?;
        let service = ephemeral_service(&host_limits, &credential, pid, &config, dogfood)?;
        let host = PreparedApiHost::new(
            listener,
            service,
            host_limits,
            Arc::new(TokioDeadlineDriver::new()),
        )
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?
        .start();
        let ready = serde_json::json!({
            "schema":"grapt.ephemeral-ready.v1",
            "pid":pid.value(),
            "endpoint":format!("http://{}", host.ready().observed_address()),
            "username":credential.username,
            "secret":credential.secret,
        });
        let publish = (|| {
            let mut output = std::io::stdout().lock();
            serde_json::to_writer(&mut output, &ready).map_err(|error| error.to_string())?;
            output
                .write_all(b"\n")
                .and_then(|()| output.flush())
                .map_err(|error| error.to_string())
        })();
        if let Err(error) = publish {
            hub.shutdown();
            let _exit = host.stop().await;
            return Err(ProductRefusal::Compose(error));
        }
        let refusal =
            super::terminal::until(control, config.serve_window_ms, Some(&mut driver)).await;
        hub.shutdown();
        let _exit = host.stop().await;
        if let Some(error) = refusal {
            return Err(ProductRefusal::Shutdown(error));
        }
        Ok(())
    })
}

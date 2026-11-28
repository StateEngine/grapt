//! A real authenticated service can bind before it has provider authority.
use super::*;

pub(super) fn initialize_if_absent(root: &Path) -> Result<(), ProductRefusal> {
    match std::fs::symlink_metadata(root) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::dogfood::first_use::initialize_empty_profile(root)
                .map(|_| ())
                .map_err(ProductRefusal::Input)
        }
        Err(error) => Err(ProductRefusal::Input(format!(
            "profile is unreadable: {:?}",
            error.kind()
        ))),
    }
}

pub(super) fn run(
    root: &Path,
    build: grapt_platform::BuildAttribution,
    admission: Admission,
    lifetime: Lifetime,
    protection: grapt_login::profile_security::ProtectionBatch,
    host_mode: HostMode,
) -> Result<LaunchReceipt, ProductRefusal> {
    run_owned(
        root, build, admission, lifetime, false, protection, host_mode,
    )
}

fn run_owned(
    root: &Path,
    build: grapt_platform::BuildAttribution,
    admission: Admission,
    lifetime: Lifetime,
    administration: bool,
    protection: grapt_login::profile_security::ProtectionBatch,
    host_mode: HostMode,
) -> Result<LaunchReceipt, ProductRefusal> {
    let config = read_config(&root.join("service.conf"), build.as_str())?;
    let credential = read_credential(&root.join("basic.credential"))?;
    let secret = credential.secret.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let hub = crate::dogfood::event_hub().map_err(ProductRefusal::Compose)?;
    crate::dogfood::stream_diagnostics::install(&hub, root);
    let limits = endpoint::provider_limits(root, &config)?;
    let (owner, receiver) = if administration {
        crate::dogfood::connection_setup::ConnectionSetup::administration(
            root,
            Arc::clone(&hub),
            limits,
            matches!(host_mode, HostMode::External),
        )
    } else {
        crate::dogfood::connection_setup::ConnectionSetup::pending(
            root,
            Arc::clone(&hub),
            limits,
            matches!(host_mode, HostMode::External),
        )
    };
    let composition = ServerComposition {
        ports: owner.ports(),
        integrations: Some(owner),
        installer: Some(Arc::new(crate::dogfood::claude_plugins::Installer::new(
            root,
        ))),
        hub,
        driver: Some(ServiceDriver::Pending(receiver)),
    };
    protection
        .finish()
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    runtime.block_on(run_ready_with_dogfood(
        root,
        config,
        credential,
        secret,
        composition,
        admission,
        lifetime,
    ))
}

pub(super) fn run_lifetime(
    service_root: &Path,
    lifetime: Lifetime,
    host_mode: HostMode,
) -> Result<LaunchReceipt, ProductRefusal> {
    let protection = grapt_login::profile_security::ProtectionBatch::begin()
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    if !matches!(&lifetime, Lifetime::Window) {
        setup::initialize_if_absent(service_root)?;
    }
    if let Some(build) = grapt_platform::PublishedConfig::empty_profile_build(service_root, None)
        .map_err(|error| lifetime.platform(error))?
    {
        let admission = Admission::Ordinary(Some(crate::startup::acquire(service_root)?));
        return setup::run(
            service_root,
            build,
            admission,
            lifetime,
            protection,
            host_mode,
        );
    }
    renewal::before_start(service_root, None)?;
    let inputs = grapt_platform::PlatformOwner::inspect_for_build(service_root, None)
        .map_err(|error| lifetime.platform(error))?;
    let admission = Admission::Ordinary(Some(crate::startup::acquire(service_root)?));
    let platform = inputs.open().map_err(|error| lifetime.platform(error))?;
    run_with_platform(
        service_root,
        platform,
        admission,
        lifetime,
        protection,
        host_mode,
    )
}

pub(super) fn run_inner(
    service_root: &Path,
    admission: Admission,
) -> Result<LaunchReceipt, ProductRefusal> {
    let protection = grapt_login::profile_security::ProtectionBatch::begin()
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    if let Some(build) = grapt_platform::PublishedConfig::empty_profile_build(
        service_root,
        admission.expected_build(),
    )
    .map_err(crate::installation::platform_refusal)?
    {
        return setup::run(
            service_root,
            build,
            admission,
            Lifetime::Window,
            protection,
            HostMode::Standalone,
        );
    }
    renewal::before_start(service_root, admission.expected_build())?;
    let (platform, admission) = admission.prepare_platform(service_root)?;
    run_with_platform(
        service_root,
        platform,
        admission,
        Lifetime::Window,
        protection,
        HostMode::Standalone,
    )
}

/// Explicit operator startup shares service election and profile publication,
/// but does not renew or open the sign-in the user is trying to repair.
pub(super) fn administration(root: &Path) -> Result<LaunchReceipt, ProductRefusal> {
    let protection = grapt_login::profile_security::ProtectionBatch::begin()
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    initialize_if_absent(root)?;
    let empty = grapt_platform::PublishedConfig::empty_profile_build(root, None)
        .map_err(crate::installation::platform_refusal)?;
    let config = if empty.is_none() {
        Some(
            grapt_platform::PublishedConfig::load_for_build(root, None)
                .map_err(crate::installation::platform_refusal)?,
        )
    } else {
        None
    };
    let admission = Admission::Ordinary(Some(crate::startup::acquire(root)?));
    let build = match config {
        Some(config) => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
            runtime
                .block_on(grapt_login::chatgpt::recover_pending_switch(root))
                .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
            let config = grapt_platform::PublishedConfig::load_for_build(
                root,
                Some(config.build().as_str()),
            )
            .map_err(crate::installation::platform_refusal)?;
            config
                .ensure_connection_store()
                .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
            config.build().clone()
        }
        None => {
            empty.ok_or_else(|| ProductRefusal::Input("profile setup header changed".into()))?
        }
    };
    run_owned(
        root,
        build,
        admission,
        Lifetime::terminal(false)?,
        true,
        protection,
        HostMode::Standalone,
    )
}

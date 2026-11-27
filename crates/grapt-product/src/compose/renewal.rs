//! Direct service starts own ChatGPT renewal, independently of CLI adapters.
use super::*;
use grapt_login::chatgpt::{self, Renewal, Session};

pub(crate) fn before_start(
    root: &Path,
    expected_build: Option<&str>,
) -> Result<(), ProductRefusal> {
    let mut config = grapt_platform::PublishedConfig::load_for_build(root, expected_build)
        .map_err(crate::installation::platform_refusal)?;
    let pending = chatgpt::has_pending_switch(root).map_err(|error| {
        ProductRefusal::Platform(format!("sign-in recovery refused: {:?}", error.kind()))
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    if pending {
        runtime
            .block_on(chatgpt::recover_pending_switch(root))
            .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
        // Explicit profiles see the recovered generation. Approved repository
        // profiles still consume their captured bytes and refuse a new account.
        config = grapt_platform::PublishedConfig::load_for_build(root, expected_build)
            .map_err(crate::installation::platform_refusal)?;
    }
    config
        .ensure_connection_store()
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    if config.chatgpt_sign_ins().next().is_none() {
        return Ok(());
    }
    let client = chatgpt::production_client()
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    runtime.block_on(renew(
        root,
        &config,
        expected_build,
        &client,
        chatgpt::ISSUER,
        &SystemClock,
    ))
}

pub(crate) async fn renew(
    root: &Path,
    config: &grapt_platform::PublishedConfig,
    expected_build: Option<&str>,
    client: &chatgpt::SignInClient,
    issuer: &str,
    clock: &(dyn grapt_types::ClockAuthority + Sync),
) -> Result<(), ProductRefusal> {
    config
        .check_build(expected_build)
        .map_err(crate::installation::platform_refusal)?;
    for (grant, files) in config.chatgpt_sign_ins() {
        let mut session = open_session(root, grant, files)?;
        if let Renewal::Deferred(error) = session
            .prepare_startup(client, issuer, clock)
            .await
            .map_err(|error| ProductRefusal::Platform(error.to_string()))?
        {
            crate::server_log::warn(format_args!(
                "grapt: ChatGPT renewal deferred; continuing with the current sign-in ({error})"
            ));
        }
    }
    Ok(())
}

fn open_session(
    root: &Path,
    grant: &grapt_platform::CredentialGrant,
    files: &chatgpt::SignInFiles,
) -> Result<Session, ProductRefusal> {
    open_session_with(root, grant, files, |authority| authority)
}

pub(crate) fn open_session_with(
    root: &Path,
    grant: &grapt_platform::CredentialGrant,
    files: &chatgpt::SignInFiles,
    wrap: impl FnOnce(
        std::sync::Arc<dyn chatgpt::SignInLockAuthority>,
    ) -> std::sync::Arc<dyn chatgpt::SignInLockAuthority>,
) -> Result<Session, ProductRefusal> {
    let session = Session::open_with_files(root, grant.account().as_str(), files.clone())
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?
        .ok_or_else(|| ProductRefusal::Platform(chatgpt::SignInError::Unfinished.to_string()))?;
    let authority = grant
        .sign_in_lock_authority(root)
        .map_err(|error| ProductRefusal::Platform(error.to_string()))?;
    Ok(match authority {
        Some(authority) => session.with_lock_authority(wrap(authority)),
        None => session,
    })
}

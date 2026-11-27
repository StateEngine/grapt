//! A service restart must recover metadata and tokens before pinning its account.
use super::*;

#[test]
fn direct_startup_finishes_a_committed_switch_before_account_pinning() -> TestResult {
    let profile = Profile::new("startup-switch-recovery", i64::MAX)?;
    let config = grapt_platform::PublishedConfig::load(&profile.0)?;
    assert!(config.ensure_connection_store()?);
    let config_before = std::fs::read(profile.0.join("grapt.jsonc"))?;
    let mut records: serde_json::Value =
        serde_json::from_slice(&grapt_login::profile_security::read_owner_only_file(
            &profile.0.join("connections.json"),
            64 * 1024,
        )?)?;
    records["connections"][0]["account"] = "account-b".into();
    let records = serde_json::to_vec(&records)?;
    let state = profile.0.join(chatgpt::STATE_FILE);
    let retained = profile.0.join("retained-state");
    std::fs::rename(&state, &retained)?;
    std::fs::create_dir(&state)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let replacement = tokens("account-b", "replacement-access", i64::MAX);
    let interrupted = runtime.block_on(chatgpt::replace_sign_in(&profile.0, &replacement, |_| {
        Ok::<_, chatgpt::SignInError>(((), vec![chatgpt::SwitchFile::Connections(records)]))
    }));
    assert!(interrupted.is_err());
    drop(runtime);
    std::fs::remove_dir(&state)?;
    std::fs::rename(&retained, &state)?;
    assert!(chatgpt::has_pending_switch(&profile.0)?);
    assert_eq!(
        chatgpt::load(&profile.0)?.ok_or("old state")?.account,
        "account-a"
    );
    assert!(crate::compose::renewal::before_start(&profile.0, Some("wrong-build")).is_err());
    assert!(chatgpt::has_pending_switch(&profile.0)?);
    assert_eq!(
        chatgpt::load(&profile.0)?.ok_or("retained state")?.account,
        "account-a"
    );
    crate::compose::renewal::before_start(&profile.0, Some("live-credential-tests"))?;
    assert!(!chatgpt::has_pending_switch(&profile.0)?);
    assert_eq!(
        chatgpt::load(&profile.0)?.ok_or("recovered state")?.account,
        "account-b"
    );
    assert_eq!(
        grapt_platform::PublishedConfig::load(&profile.0)?
            .credentials()
            .account()
            .as_str(),
        "account-b"
    );
    assert_eq!(std::fs::read(profile.0.join("grapt.jsonc"))?, config_before);
    Ok(())
}

use super::*;

fn fixture() -> Result<(PromptCacheKey, ProjectId, ResolvedModelTarget), String> {
    let session: SessionId = "ses_scope".parse().map_err(|_| "session")?;
    let own = PromptCacheKey::for_session(&session).map_err(|error| error.to_string())?;
    let project = ProjectId::new("prj_scope".to_owned()).ok_or("project")?;
    let target = crate::dogfood::production::tests::projection_target()?;
    Ok((own, project, target))
}

#[test]
fn session_scope_is_the_sessions_own_key_byte_for_byte() -> Result<(), String> {
    let (own, project, target) = fixture()?;
    let key = scoped(PromptCacheScope::Session, own.clone(), &project, &target);
    assert_eq!(key, own);
    assert_eq!(key.session_header(), own.as_str());
    Ok(())
}

#[test]
fn project_scopes_share_the_key_and_only_one_shares_the_header() -> Result<(), String> {
    let (own, project, target) = fixture()?;
    let shared = PromptCacheKey::for_project(
        project.as_str(),
        target.provider().as_str(),
        target.model().as_str(),
    );
    let key = scoped(PromptCacheScope::ProjectKey, own.clone(), &project, &target);
    assert_eq!(key.as_str(), shared.as_str());
    assert_eq!(key.session_header(), own.as_str());
    let both = scoped(
        PromptCacheScope::ProjectKeyAndHeader,
        own.clone(),
        &project,
        &target,
    );
    assert_eq!(both.as_str(), shared.as_str());
    assert_eq!(both.session_header(), shared.as_str());
    Ok(())
}

#[test]
fn the_profile_scope_reads_absent_as_session_and_refuses_unknown_tokens() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("grapt-cache-scope-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let path = root.join("service.conf");
    let base = "slot=product\nprovider_mode=production\n";
    for (extra, expected) in [
        ("", Some(PromptCacheScope::Session)),
        (
            "prompt_cache_scope=session\n",
            Some(PromptCacheScope::Session),
        ),
        (
            "prompt_cache_scope=project-key\n",
            Some(PromptCacheScope::ProjectKey),
        ),
        (
            "prompt_cache_scope=project-key-and-header\n",
            Some(PromptCacheScope::ProjectKeyAndHeader),
        ),
        ("prompt_cache_scope=everything\n", None),
    ] {
        std::fs::write(&path, format!("{base}{extra}")).map_err(|error| error.to_string())?;
        let read = read_prompt_cache_scope(&path).ok();
        assert_eq!(read, expected, "{extra:?}");
    }
    std::fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
    Ok(())
}

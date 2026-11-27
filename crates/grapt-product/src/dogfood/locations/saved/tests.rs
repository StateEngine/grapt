use super::*;
use crate::dogfood::support::decode;
use grapt_session::SessionSeed;
use grapt_types::*;
use serde_json::json;

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

#[tokio::test]
async fn canonical_session_location_refuses_foreign_checkout_without_running_git(
) -> Result<(), Box<dyn std::error::Error>> {
    let ids = grapt_store::OsRandomIds::seed()?;
    let root = std::env::temp_dir().join(format!("saved-location-{}", ids.mint().as_str()));
    std::fs::create_dir(&root)?;
    let result = check(&root).await;
    std::fs::remove_dir_all(&root)?;
    result
}

async fn check(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let repo = root.join("repo");
    let nested = repo.join("nested");
    let foreign = root.join("foreign");
    std::fs::create_dir(&repo)?;
    std::fs::create_dir(&nested)?;
    std::fs::create_dir(&foreign)?;
    // A marker is sufficient for the filesystem boundary; Git cannot resolve
    // this deliberately invalid repository. The Session remains authoritative.
    std::fs::write(repo.join(".git"), b"not a usable Git repository")?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        root,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let project = ProjectId::new("saved-project").ok_or("project")?;
    let directory = nested.to_str().ok_or("directory")?;
    let authorities = DeterministicAuthorities::new(&*app.clock, &*app.ids);
    let ports = SessionPorts::new(Arc::clone(&app.inbox), &authorities);
    let session = ports
        .create(&SessionSeed::new(project.clone(), directory, "saved")?)?
        .session;
    for requested in [&nested, &repo] {
        let query = decode(json!({"session":session.as_str(),"location":{"directory":requested}}))?;
        let resolved = resolve_for(&app, &query).await?;
        assert_eq!(resolved.project, project);
        assert_eq!(resolved.directory, directory);
        assert_eq!(Path::new(&resolved.project_directory), repo);
    }
    for value in [
        json!({"session":session.as_str()}),
        json!({"session":session.as_str(),"location":{"directory":"relative"}}),
        json!({"session":session.as_str(),"location":{"directory":foreign}}),
        json!({"session":session.as_str(),"location":{"directory":root}}),
        json!({"session":session.as_str(),"location":{"directory":nested,"workspace":"wrk_foreign"}}),
        json!({"session":"ses_missing","location":{"directory":nested}}),
    ] {
        let query = decode(value)?;
        assert!(resolve_for(&app, &query).await.is_err());
    }
    // The unbound route must still report unreadable metadata, never global.
    let query = decode(json!({"location":{"directory":nested}}))?;
    assert!(resolve_for(&app, &query).await.is_err());
    Ok(())
}

use super::*;
use crate::dogfood::ServerApplication;
use grapt_runtime::SessionPorts;
use grapt_session::SessionSeed;
use grapt_types::{
    AccountId, BaseModelId, CredentialVersion, ModelCatalog, ModelCatalogRecord,
    ModelCatalogVersion, ModelFamilyId, ModelId, ModelRegistry, ModelRouteId, PolicyVersion,
    ProviderId, ResolvedModelTarget, RoutingRequest,
};
use grapt_types::{DeterministicAuthorities, IdAuthority};
use std::path::PathBuf;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[path = "background_child_tests.rs"]
mod background;

#[path = "subagent_history_tests.rs"]
mod history;
#[path = "subagent_worktree_tests.rs"]
mod worktree;

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
        let root = std::env::temp_dir().join(format!("subagent-admit-{}", ids.mint().as_str()));
        std::fs::create_dir(&root)?;
        let root = Self(root);
        let initialized = std::process::Command::new("git")
            .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
            .args(["init", "--quiet"])
            .arg(&root.0)
            .status()?;
        if !initialized.success() {
            return Err("fixture Git boundary could not be initialized".into());
        }
        let configuration = root.0.join("service.conf");
        std::fs::write(&configuration,
            "slot=product\ninstance=subagent-admit\nenvironment=acceptance\nversion=0.1.0\nserve_window_ms=1000\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=30000\nprovider_active_attempts=1\npermission_preset=author\n")?;
        crate::input::read_config(&configuration, "subagent-fixture")?;
        Ok(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The parent Session a subagent is admitted under.
fn a_parent(
    application: &ServerApplication,
    directory: &str,
) -> Result<SessionId, Box<dyn std::error::Error>> {
    // Any valid project identity: what this witness is about is the
    // lineage the child copies from the parent's record, not how the
    // parent's placement was discovered.
    let project = grapt_types::ProjectId::new("prj_fixture".to_owned()).ok_or("project")?;
    let seed = SessionSeed::new(project, directory.to_owned(), "parent")?
        .with_agent("agt_live_headless1")?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let created = SessionPorts::new(Arc::clone(&application.inbox), &authorities).create(&seed)?;
    Ok(created.session)
}

#[test]
fn admitting_a_child_records_its_lineage_without_inventing_a_fork() -> TestResult {
    let root = Root::new()?;
    let hub = crate::dogfood::event_hub()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        Arc::clone(&hub),
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let directory = root.0.to_str().ok_or("fixture path encoding")?;
    let parent = a_parent(&app, directory)?;
    let agent = AgentId::new("agt_live_headless1".to_owned()).ok_or("agent")?;

    let child = admit(&app, &parent, &agent, "summarise the diff", None)?;

    // THREE DISTINCT COORDINATES. The oracle keyed the operation by child
    // session id; if any two of these collapsed, that impersonation would
    // be back.
    assert_ne!(child.session, parent, "the child is its own Session");
    assert_ne!(
        child.operation.as_str(),
        child.session.as_str(),
        "the operation address is not the child Session id"
    );

    // LINEAGE, through with_parent — the first production caller it has.
    let record = app.get_record(&child.session)?;
    assert_eq!(
        record.parent.as_ref(),
        Some(&parent),
        "the child records the Session it was launched from"
    );
    assert_eq!(record.preset.as_deref(), Some("author"));

    // AND NOT A FORK. A fork inherits capped parent history through
    // `session.forked.2`; a subagent starts empty with a recorded ancestor.
    // Emitting one here would claim a transcript the child does not have.
    let held = app.lock_inbox();
    let forked: i64 = held.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type LIKE 'session.forked%'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(forked, 0, "no fork event was emitted for a subagent child");

    // The operation is ADMITTED, holding both ends of the lineage.
    let (mode, status, stored_parent, stored_child): (String, String, String, String) =
        held.store().connection().query_row(
            "SELECT mode, status, parent_session_id, child_session_id
                 FROM subagent_operation WHERE operation_id = ?1",
            [child.operation.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    assert_eq!(mode, "foreground");
    assert_eq!(status, "admitted", "a freshly opened operation is admitted");
    assert_eq!(stored_parent, parent.as_str());
    assert_eq!(stored_child, child.session.as_str());
    Ok(())
}

#[test]
fn missing_or_non_regular_service_config_refuses_before_child_creation() -> TestResult {
    for non_regular in [false, true] {
        let root = Root::new()?;
        let hub = crate::dogfood::event_hub()?;
        let (app, _driver) = crate::dogfood::test_provider::open(
            &root.0,
            grapt_store::EventStore::open_in_memory()?,
            &target()?,
            hub,
            crate::dogfood::plugins::PluginRuntime::empty(),
        )?;
        let parent = a_parent(&app, root.0.to_str().ok_or("fixture path encoding")?)?;
        let agent = AgentId::new("agt_live_headless1").ok_or("agent")?;
        let count_events = || -> Result<i64, rusqlite::Error> {
            app.lock_inbox().store().connection().query_row(
                "SELECT COUNT(*) FROM event",
                [],
                |row| row.get(0),
            )
        };
        let before = count_events()?;
        let configuration = root.0.join("service.conf");
        std::fs::remove_file(&configuration)?;
        if non_regular {
            std::fs::create_dir(&configuration)?;
        }
        let error = admit(&app, &parent, &agent, "summarise the diff", None)
            .err()
            .ok_or("missing owner configuration admitted a child")?;
        assert!(error.contains("service.conf"), "{error}");
        assert_eq!(count_events()?, before, "refusal wrote child events");
    }
    Ok(())
}

#[test]
fn background_admission_records_its_mode_and_keeps_parent_placement() -> TestResult {
    let root = Root::new()?;
    let hub = crate::dogfood::event_hub()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        hub,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let directory = root.0.to_str().ok_or("fixture path encoding")?;
    let parent = a_parent(&app, directory)?;
    let agent = AgentId::new("agt_live_headless1").ok_or("agent")?;
    let child = admit_with_options(
        &app,
        &parent,
        &agent,
        "background task",
        None,
        &ChildOptions::new(SubagentMode::Background),
    )?;
    let record = app.get_record(&child.session)?;
    assert_eq!(record.parent.as_ref(), Some(&parent));
    assert_eq!(record.placement.directory(), directory);
    assert_eq!(record.preset.as_deref(), Some("author"));
    let held = app.lock_inbox();
    let mode: String = held.store().connection().query_row(
        "SELECT mode FROM subagent_operation WHERE operation_id = ?1",
        [child.operation.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(mode, "background");
    Ok(())
}

#[test]
fn named_child_uses_its_model_and_cannot_widen_ancestor_tools() -> TestResult {
    let root = Root::new()?;
    let agents = root.0.join(".grapt/agents");
    std::fs::create_dir_all(&agents)?;
    std::fs::write(agents.join("reader.toml"), "name='reader'\ndescription='Read sources'\ndeveloper_instructions='Explain the code'\nmodel='fixture/fixture'\ntools=['read']\n")?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let parent = a_parent(&app, root.0.to_str().ok_or("path")?)?;
    let reader = AgentId::new("reader").ok_or("agent")?;
    let child = admit(&app, &parent, &reader, "explain", None)?;
    let record = app.get_record(&child.session)?;
    assert_eq!(record.agent.as_deref(), Some("reader"));
    assert_eq!(
        record.model,
        Some(serde_json::json!({"providerID":"fixture","id":"fixture"}))
    );
    let worker = AgentId::new("worker").ok_or("worker")?;
    let grandchild = admit(&app, &child.session, &worker, "nested task", None)?;
    let record = app.get_record(&grandchild.session)?;
    let policy =
        crate::dogfood::permission_preset::effective(&root.0, &record, |id| app.get_record(id))?;
    assert!(policy.permits_agent_tool("read"));
    assert!(!policy.permits_agent_tool("exec_command"));
    assert!(policy.withholds("exec_command"));
    let resource = agents
        .join("new-definition.toml")
        .to_string_lossy()
        .into_owned();
    let check = grapt_permission::PermissionConsequence {
        action: "fs.create".into(),
        resource: resource.clone(),
    };
    assert_eq!(
        grapt_permission::preset::decide("fs.create", &[&resource], &policy.layers(None, &[check]))
            .effect,
        grapt_permission::preset::Effect::Ask
    );
    let unknown = AgentId::new("missing").ok_or("missing")?;
    assert!(admit(&app, &parent, &unknown, "must refuse", None).is_err());
    Ok(())
}

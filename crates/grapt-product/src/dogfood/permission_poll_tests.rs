use super::*;
use grapt_types::*;
use std::{
    sync::{mpsc, Arc},
    time::Duration,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[path = "permission_view_behavior.rs"]
mod behavior;

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

struct Root(std::path::PathBuf);

impl Root {
    fn new() -> TestResultRoot {
        let ids = grapt_store::OsRandomIds::seed()?;
        let path = std::env::temp_dir().join(format!("permission-poll-{}", ids.mint().as_str()));
        std::fs::create_dir(&path)?;
        let root = Self(path);
        if !std::process::Command::new("git")
            .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
            .args(["init", "--quiet"])
            .arg(&root.0)
            .status()?
            .success()
        {
            return Err("permission polling fixture Git initialization failed".into());
        }
        Ok(root)
    }
}

type TestResultRoot = Result<Root, Box<dyn std::error::Error>>;

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn permission_poll_finishes_before_a_staged_commit_releases_inbox() -> TestResult {
    let root = Root::new()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open(&root.0.join("database.sqlite3"), "permission-view")?,
        &target()?,
        super::super::event_hub()?,
        super::super::plugins::PluginRuntime::empty(),
    )?;
    let project = ProjectId::new("prj_poll").ok_or("project")?;
    let seed = grapt_session::SessionSeed::new(
        project.clone(),
        root.0.to_str().ok_or("root encoding")?.to_owned(),
        "before",
    )?;
    let authorities = DeterministicAuthorities::new(&*app.clock, &*app.ids);
    let session = grapt_runtime::SessionPorts::new(Arc::clone(&app.inbox), &authorities)
        .create(&seed)?
        .session;
    let client = AuthenticatedClientId::new("client_poll").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "subject_poll",
        client: &client,
    };
    let permission: PermissionId = "per_poll".parse()?;
    let event: EventId = "evt_poll_permission".parse()?;
    let agent = AgentId::new("agt_poll").ok_or("agent")?;
    {
        let mut permissions = app.lock_permissions();
        let mut inbox = app.lock_inbox();
        assert_eq!(
            permissions.create(
                inbox.store_mut(),
                &owner,
                &event,
                &permission,
                &session,
                &agent,
                &[],
                &project,
                "read",
                &["file.txt".to_owned()],
                None,
                None,
                None,
                100,
            )?,
            PermissionCreateEffect::Ask
        );
    }
    let initial = polling_page(&app, &owner, Some(&session))
        .map_err(|error| format!("initial permission poll: {error:?}"))?;
    assert_eq!(initial.requests.len(), 1);
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (answer_tx, answer_rx) = mpsc::sync_channel(1);
    let writer_app = Arc::clone(&app);
    let writer_session = session.clone();
    let writer = std::thread::spawn(move || -> Result<(), String> {
        let mut inbox = writer_app.lock_inbox();
        let aggregate = grapt_store::AggregateId::Session(writer_session.clone());
        let writer = inbox
            .store()
            .fencing_token(&aggregate)
            .map_err(|e| e.to_string())?;
        let tx = inbox
            .store_mut()
            .governed_transaction()
            .map_err(|e| e.to_string())?;
        let event = "evt_poll_rename"
            .parse::<EventId>()
            .map_err(|e| e.to_string())?;
        let version = EventVersion::new(1).ok_or("version")?;
        let data = json!({"sessionID":writer_session.as_str(),"title":"after"});
        let mut projector = grapt_session::SessionListProjector;
        grapt_store::EventStore::append_within(
            &tx,
            grapt_store::AppendInput {
                id: &event,
                aggregate,
                writer,
                kind_name: "session.renamed",
                version,
                created_ms: 101,
                data: &data,
            },
            &mut [&mut projector],
        )
        .map_err(|e| e.to_string())?;
        entered_tx.send(()).map_err(|e| e.to_string())?;
        // Bounded real transaction, staged and holding the production Inbox.
        // No sleep or ambient clock; failure cleanup releases before joining.
        release_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?.discard();
        Ok(())
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(5));
    let reader_app = Arc::clone(&app);
    let reader_session = session.clone();
    let reader_client = client.clone();
    let reader = std::thread::spawn(move || {
        let owner = PermissionOwner {
            subject: "subject_poll",
            client: &reader_client,
        };
        let answer = polling_page(&reader_app, &owner, Some(&reader_session))
            .map_err(|error| format!("permission poll failed: {error:?}"));
        let _ = answer_tx.send(answer);
    });
    let before_release = answer_rx.recv_timeout(Duration::from_secs(1));
    let _ = release_tx.send(());
    let writer_result = writer.join().map_err(|_| "writer panicked")?;
    reader.join().map_err(|_| "reader panicked")?;
    writer_result?;
    entered?;
    let answer =
        before_release.map_err(|_| "permission poll waited for staged COMMIT/Inbox release")??;
    assert_eq!(answer.requests.len(), 1);
    assert_eq!(answer.requests[0].id, permission);
    assert_eq!(app.get_record(&session)?.title, "after");
    Ok(())
}

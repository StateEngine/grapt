//! Session-form recovery keeps the Product's canonical Session writer and CAS.
use super::*;
use grapt_runtime::SessionPorts;
use grapt_types::*;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

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
    fn new() -> TestResult<Self> {
        let ids = grapt_store::OsRandomIds::seed()?;
        let path =
            std::env::temp_dir().join(format!("session-form-recovery-{}", ids.mint().as_str()));
        std::fs::create_dir(&path)?;
        let root = Self(path);
        if !std::process::Command::new("git")
            .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
            .args(["init", "--quiet"])
            .arg(&root.0)
            .status()?
            .success()
        {
            return Err("form fixture Git boundary failed".into());
        }
        Ok(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open(root: &Path) -> TestResult<Arc<DogfoodApplication>> {
    let (application, _driver) = crate::dogfood::test_provider::open(
        root,
        grapt_store::EventStore::open(&root.join("database.sqlite3"), "session-form-recovery")?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    Ok(application)
}

fn session(app: &DogfoodApplication, root: &Path) -> TestResult<SessionId> {
    let seed = grapt_session::SessionSeed::new(
        app.catalog_snapshot().project.clone(),
        root.to_str().ok_or("path encoding")?.to_owned(),
        "form recovery",
    )?;
    let authorities = DeterministicAuthorities::new(&*app.clock, &*app.ids);
    Ok(SessionPorts::new(Arc::clone(&app.inbox), &authorities)
        .create(&seed)?
        .session)
}

fn create_form(app: &DogfoodApplication, owner: &SessionId, name: &str) -> TestResult<FormId> {
    let id: FormId = format!("frm_recovery_{name}").parse()?;
    let request = decode(json!({"id":id.as_str(), "title":"Branch?", "fields":[
        {"key":"branch", "type":"string", "required":true}]}))?;
    create(app, owner.clone(), request).map_err(|error| format!("create: {error:?}"))?;
    Ok(id)
}

fn answer(
    app: &DogfoodApplication,
    owner: &SessionId,
    form: &FormId,
) -> Result<NoContent, FormReplyApplicationError> {
    let request = decode(json!({"answer":{"branch":"main"}}))
        .map_err(|error| FormReplyApplicationError::InvalidRequest(invalid(error)))?;
    reply(app, owner.clone(), form.clone(), request)
}

fn facts(app: &DogfoodApplication, owner: &SessionId) -> TestResult<Vec<String>> {
    let inbox = app.lock_inbox();
    let mut query = inbox.store().connection().prepare(
        "SELECT type FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND type LIKE 'form.%' ORDER BY seq")?;
    let rows = query
        .query_map([owner.as_str()], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn epoch(app: &DogfoodApplication, owner: &SessionId) -> TestResult<i64> {
    Ok(app.lock_inbox().store().connection().query_row(
        "SELECT epoch FROM aggregate_writer WHERE aggregate_family='session' AND aggregate_id=?1",
        [owner.as_str()],
        |row| row.get(0),
    )?)
}

#[test]
fn pending_session_forms_answer_and_cancel_after_reopen_once() -> TestResult {
    let root = Root::new()?;
    let first = open(&root.0)?;
    let owner = session(&first, &root.0)?;
    let answered = create_form(&first, &owner, "answer")?;
    let cancelled = create_form(&first, &owner, "cancel")?;
    let previous = epoch(&first, &owner)?;
    drop(first);
    let app = open(&root.0)?;
    assert!(matches!(
        app.lock_inbox()
            .store()
            .fencing_token(&grapt_store::AggregateId::Session(owner.clone())),
        Err(grapt_store::WriteError::NotElected { .. })
    ));
    answer(&app, &owner, &answered).map_err(|error| format!("reopened reply: {error:?}"))?;
    cancel(&app, owner.clone(), cancelled.clone())
        .map_err(|error| format!("reopened cancel: {error:?}"))?;
    assert_eq!(epoch(&app, &owner)?, previous + 1);
    assert_eq!(
        facts(&app, &owner)?,
        [
            "form.created.1",
            "form.created.1",
            "form.replied.1",
            "form.cancelled.1"
        ]
    );
    assert!(matches!(
        answer(&app, &owner, &answered),
        Err(FormReplyApplicationError::FormAlreadySettled(_))
    ));
    drop(app);
    let reopened = open(&root.0)?;
    assert!(matches!(
        answer(&reopened, &owner, &answered),
        Err(FormReplyApplicationError::FormAlreadySettled(_))
    ));
    assert!(matches!(
        cancel(&reopened, owner.clone(), cancelled),
        Err(FormCancelApplicationError::FormAlreadySettled(_))
    ));
    assert_eq!(
        epoch(&reopened, &owner)?,
        previous + 1,
        "terminal replay must not acquire another writer"
    );
    Ok(())
}

#[test]
fn cancelled_form_can_be_the_first_recovered_session_write() -> TestResult {
    let root = Root::new()?;
    let app = open(&root.0)?;
    let owner = session(&app, &root.0)?;
    let form = create_form(&app, &owner, "cancel_first")?;
    drop(app);
    let app = open(&root.0)?;
    cancel(&app, owner.clone(), form)
        .map_err(|error| format!("first recovered cancel: {error:?}"))?;
    assert_eq!(facts(&app, &owner)?, ["form.created.1", "form.cancelled.1"]);
    Ok(())
}

#[test]
fn invalid_answer_after_reopen_leaves_writer_and_pending_form_untouched() -> TestResult {
    let root = Root::new()?;
    let app = open(&root.0)?;
    let owner = session(&app, &root.0)?;
    let form = create_form(&app, &owner, "invalid")?;
    let previous = epoch(&app, &owner)?;
    drop(app);
    let app = open(&root.0)?;
    let request = decode(json!({"answer":{"branch":42}}))?;
    assert!(matches!(
        reply(&app, owner.clone(), form.clone(), request),
        Err(FormReplyApplicationError::FormInvalidAnswer(_))
    ));
    assert_eq!(epoch(&app, &owner)?, previous);
    assert_eq!(facts(&app, &owner)?, ["form.created.1"]);
    answer(&app, &owner, &form).map_err(|error| format!("corrected reply: {error:?}"))?;
    Ok(())
}

#[test]
fn a_stale_session_writer_cannot_resurrect_by_replying_or_cancelling() -> TestResult {
    let root = Root::new()?;
    let app = open(&root.0)?;
    let owner = session(&app, &root.0)?;
    let form = create_form(&app, &owner, "fenced")?;
    let mut winner =
        grapt_store::EventStore::open(&root.0.join("database.sqlite3"), "winning-owner")?;
    let aggregate = grapt_store::AggregateId::Session(owner.clone());
    let elected = winner.elect_writer(&aggregate)?.epoch();
    assert!(matches!(
        answer(&app, &owner, &form),
        Err(FormReplyApplicationError::InvalidRequest(_))
    ));
    assert!(matches!(
        cancel(&app, owner.clone(), form.clone()),
        Err(FormCancelApplicationError::InvalidRequest(_))
    ));
    assert!(matches!(
        answer(&app, &owner, &form),
        Err(FormReplyApplicationError::InvalidRequest(_))
    ));
    assert_eq!(epoch(&app, &owner)?, i64::from(elected.value()));
    assert_eq!(facts(&app, &owner)?, ["form.created.1"]);
    assert_eq!(
        grapt_form::FormService::answer_on(
            &mut winner,
            &"evt_winning_form_reply".parse()?,
            &form,
            &json!({"branch":"winner"}),
            100
        )?,
        CasOutcome::Applied
    );
    assert_eq!(facts(&app, &owner)?, ["form.created.1", "form.replied.1"]);
    Ok(())
}

#[test]
fn recovered_answer_and_cancel_race_commits_one_terminal_fact() -> TestResult {
    let root = Root::new()?;
    let app = open(&root.0)?;
    let owner = session(&app, &root.0)?;
    let form = create_form(&app, &owner, "race")?;
    let previous = epoch(&app, &owner)?;
    drop(app);
    let app = open(&root.0)?;
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let reply = scope.spawn(|| {
            barrier.wait();
            match answer(&app, &owner, &form) {
                Ok(_) => Ok(true),
                Err(FormReplyApplicationError::FormAlreadySettled(_)) => Ok(false),
                Err(error) => Err(format!("racing reply: {error:?}")),
            }
        });
        let cancel = scope.spawn(|| {
            barrier.wait();
            match cancel(&app, owner.clone(), form.clone()) {
                Ok(_) => Ok(true),
                Err(FormCancelApplicationError::FormAlreadySettled(_)) => Ok(false),
                Err(error) => Err(format!("racing cancel: {error:?}")),
            }
        });
        Ok::<_, String>((
            reply.join().map_err(|_| "reply panicked")??,
            cancel.join().map_err(|_| "cancel panicked")??,
        ))
    })?;
    assert_ne!(results.0, results.1, "exactly one settlement must win");
    assert_eq!(epoch(&app, &owner)?, previous + 1);
    assert_eq!(facts(&app, &owner)?.len(), 2);
    drop(app);
    let app = open(&root.0)?;
    assert!(matches!(
        answer(&app, &owner, &form),
        Err(FormReplyApplicationError::FormAlreadySettled(_))
    ));
    assert_eq!(facts(&app, &owner)?.len(), 2);
    Ok(())
}

#[test]
fn corrupt_pending_form_is_refused_without_acquiring_a_writer() -> TestResult {
    let root = Root::new()?;
    let app = open(&root.0)?;
    let owner = session(&app, &root.0)?;
    let form = create_form(&app, &owner, "corrupt")?;
    let previous = epoch(&app, &owner)?;
    drop(app);
    let app = open(&root.0)?;
    app.lock_inbox().store().connection().execute(
        "UPDATE form_request SET title='corrupt projection' WHERE id=?1",
        [form.as_str()],
    )?;
    assert!(matches!(
        answer(&app, &owner, &form),
        Err(FormReplyApplicationError::InvalidRequest(_))
    ));
    assert!(matches!(
        cancel(&app, owner.clone(), form),
        Err(FormCancelApplicationError::InvalidRequest(_))
    ));
    assert_eq!(
        epoch(&app, &owner)?,
        previous,
        "corruption must not elect a writer"
    );
    assert_eq!(facts(&app, &owner)?, ["form.created.1"]);
    Ok(())
}

#[test]
fn session_form_routes_publish_committed_facts_without_replaying_refusals() -> TestResult {
    let root = Root::new()?;
    let app = open(&root.0)?;
    let owner = session(&app, &root.0)?;
    let feed = app.hub.subscribe_host()?;
    let mut expected = Vec::new();
    for cancelled in [false, true] {
        let id = create_form(
            &app,
            &owner,
            if cancelled {
                "cancel_feed"
            } else {
                "answer_feed"
            },
        )?;
        expected.push(json!({"type":"form.created","id":id.as_str()}));
        if cancelled {
            cancel(&app, owner.clone(), id.clone()).map_err(|e| format!("cancel: {e:?}"))?;
        } else {
            answer(&app, &owner, &id).map_err(|e| format!("reply: {e:?}"))?;
        }
        expected.push(
            json!({"type":if cancelled {"form.cancelled"} else {"form.replied"},"id":id.as_str()}),
        );
        assert!(cancel(&app, owner.clone(), id).is_err());
    }
    // next_frame is a blocking host adapter. Close after the writes, then
    // drain its bounded queue so an absent notification fails without hanging.
    feed.cancel();
    let mut actual = Vec::new();
    let mut frames = 0;
    while let Some(frame) = feed.next_frame()? {
        frames += 1;
        assert!(frames <= 16, "unexpected form fixture frame count");
        let text = std::str::from_utf8(&frame)?;
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data:"))
            .ok_or("SSE data")?;
        let wire: Value = serde_json::from_str(data.trim())?;
        if !wire["type"]
            .as_str()
            .is_some_and(|kind| kind.starts_with("form."))
        {
            continue;
        }
        assert_eq!(wire["durable"]["aggregateID"], owner.as_str());
        assert_eq!(wire["durable"]["version"], 1);
        assert_eq!(wire["data"]["sessionID"], owner.as_str());
        let id = if wire["type"] == "form.created" {
            &wire["data"]["form"]["id"]
        } else {
            &wire["data"]["id"]
        };
        actual.push(json!({"type":wire["type"],"id":id}));
    }
    assert_eq!(
        actual, expected,
        "missing, repeated or uncommitted form notification"
    );
    Ok(())
}

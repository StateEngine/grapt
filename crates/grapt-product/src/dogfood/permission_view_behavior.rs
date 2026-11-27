use super::*;

#[path = "permission_preview_tests.rs"]
mod preview_tests;

fn application(
    root: &Root,
    file: bool,
) -> Result<Arc<DogfoodApplication>, Box<dyn std::error::Error>> {
    let store = if file {
        grapt_store::EventStore::open(&root.0.join("database.sqlite3"), "view-test")?
    } else {
        grapt_store::EventStore::open_in_memory()?
    };
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        store,
        &target()?,
        super::super::super::event_hub()?,
        super::super::super::plugins::PluginRuntime::empty(),
    )?;
    Ok(app)
}

fn session(app: &DogfoodApplication, root: &Root) -> Result<SessionId, Box<dyn std::error::Error>> {
    let seed = grapt_session::SessionSeed::new(
        ProjectId::new("prj_view").ok_or("project")?,
        root.0.to_str().ok_or("root encoding")?.to_owned(),
        "view",
    )?;
    let authorities = DeterministicAuthorities::new(&*app.clock, &*app.ids);
    Ok(
        grapt_runtime::SessionPorts::new(Arc::clone(&app.inbox), &authorities)
            .create(&seed)?
            .session,
    )
}

fn ask(
    app: &DogfoodApplication,
    owner: &PermissionOwner<'_>,
    session: &SessionId,
    index: usize,
    metadata: Option<&serde_json::Value>,
) -> Result<PermissionId, Box<dyn std::error::Error>> {
    let id: PermissionId = format!("per_view{index}").parse()?;
    let event: EventId = format!("evt_view{index}").parse()?;
    let agent = AgentId::new("agt_view").ok_or("agent")?;
    let project = ProjectId::new("prj_view").ok_or("project")?;
    let mut permissions = app.lock_permissions();
    let mut inbox = app.lock_inbox();
    assert_eq!(
        permissions.create(
            inbox.store_mut(),
            owner,
            &event,
            &id,
            session,
            &agent,
            &[],
            &project,
            "read",
            &["file.txt".to_owned()],
            None,
            metadata,
            None,
            100
        )?,
        PermissionCreateEffect::Ask
    );
    Ok(id)
}

fn parity(
    app: &DogfoodApplication,
    owner: &PermissionOwner<'_>,
    session: Option<&SessionId>,
) -> TestResult {
    let accelerated = app
        .poll_committed_permissions(owner, session)
        .ok_or("view unavailable")?
        .map_err(|error| format!("view: {error:?}"))?;
    let durable = {
        let permissions = app.lock_permissions();
        let inbox = app.lock_inbox();
        permissions.list(inbox.store(), owner, session)?
    };
    assert_eq!(accelerated, durable);
    Ok(())
}

#[test]
fn owner_session_ordering_and_has_more_match_durable_queries() -> TestResult {
    let root = Root::new()?;
    let app = application(&root, false)?;
    let session_a = session(&app, &root)?;
    let session_b = session(&app, &root)?;
    let client = AuthenticatedClientId::new("client_view").ok_or("client")?;
    let wrong_client = AuthenticatedClientId::new("client_other").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "subject_view",
        client: &client,
    };
    for index in 0..130 {
        ask(&app, &owner, &session_a, index, None)?;
    }
    ask(&app, &owner, &session_b, 130, None)?;
    for session in [None, Some(&session_a), Some(&session_b)] {
        parity(&app, &owner, session)?;
    }
    parity(
        &app,
        &PermissionOwner {
            subject: "other",
            client: &client,
        },
        None,
    )?;
    parity(
        &app,
        &PermissionOwner {
            subject: "subject_view",
            client: &wrong_client,
        },
        Some(&session_a),
    )?;
    let page =
        polling_page(&app, &owner, Some(&session_a)).map_err(|error| format!("{error:?}"))?;
    assert_eq!(page.requests.len(), 128);
    assert!(page.has_more);
    assert_eq!(page.requests[0].id.as_str(), "per_view129");
    assert!(
        app.permission_view
            .allocation_bytes()
            .ok_or("generation missing")?
            < 1_048_576
    );
    Ok(())
}

#[test]
fn capacity_fallback_preserves_commit_startup_reads_and_recovers_on_cancel() -> TestResult {
    let root = Root::new()?;
    let mut app = application(&root, true)?;
    let session = session(&app, &root)?;
    let client = AuthenticatedClientId::new("client_view").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "subject_view",
        client: &client,
    };
    let metadata = json!({"large":"x".repeat(1_048_576)});
    let id = ask(&app, &owner, &session, 500, Some(&metadata))?;
    assert!(
        app.permission_view.allocation_bytes().is_none(),
        "over-capacity snapshot stayed available"
    );
    assert_eq!(
        polling_page(&app, &owner, Some(&session))
            .map_err(|error| format!("{error:?}"))?
            .requests[0]
            .metadata,
        Some(metadata)
    );
    drop(app);
    app = application(&root, true)?;
    assert!(app.permission_view.allocation_bytes().is_none());
    assert_eq!(
        polling_page(&app, &owner, Some(&session))
            .map_err(|error| format!("{error:?}"))?
            .requests[0]
            .id,
        id
    );
    // Restart lost process-local writer ownership; the HTTP mutation path
    // obtains it before entering this same borrowed-store cancellation seam.
    app.ensure_session_writer(&session)?;
    {
        let mut permissions = app.lock_permissions();
        let mut inbox = app.lock_inbox();
        assert!(permissions
            .cancel_tool(
                inbox.store_mut(),
                &owner,
                &session,
                &id,
                &"evt_cancel_view".parse()?,
                101
            )?
            .is_some());
    }
    assert!(
        app.permission_view.allocation_bytes().is_some(),
        "terminal row did not restore available snapshot"
    );
    assert!(polling_page(&app, &owner, Some(&session))
        .map_err(|error| format!("{error:?}"))?
        .requests
        .is_empty());
    Ok(())
}

#[test]
fn deletion_and_corrupt_lifecycle_keep_existing_session_refusals() -> TestResult {
    let root = Root::new()?;
    let app = application(&root, false)?;
    let session = session(&app, &root)?;
    let client = AuthenticatedClientId::new("client_view").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "subject_view",
        client: &client,
    };
    ask(&app, &owner, &session, 700, None)?;
    {
        let authorities = DeterministicAuthorities::new(&*app.clock, &*app.ids);
        let mut inbox = app.lock_inbox();
        grapt_session::delete_session(inbox.store_mut(), &authorities, &session)?;
    }
    assert!(app.get_record(&session).is_err());
    assert!(polling_page(&app, &owner, Some(&session)).is_err());
    parity(&app, &owner, None)?;
    let corrupt_session = self::session(&app, &root)?;
    {
        let mut inbox = app.lock_inbox();
        let aggregate = grapt_store::AggregateId::Session(corrupt_session.clone());
        let writer = inbox.store().fencing_token(&aggregate)?;
        let tx = inbox.store_mut().governed_transaction()?;
        // A deliberately corrupt lifecycle fact, with no semantic projector.
        grapt_store::EventStore::append_within(
            &tx,
            grapt_store::AppendInput {
                id: &"evt_corrupt_view".parse()?,
                aggregate,
                writer,
                kind_name: "session.renamed",
                version: EventVersion::new(1).ok_or("version")?,
                created_ms: 102,
                data: &json!({"sessionID":corrupt_session.as_str()}),
            },
            &mut [],
        )?;
        tx.commit()?.discard();
    }
    assert!(
        app.permission_view.allocation_bytes().is_none(),
        "corrupt lifecycle was treated as a valid snapshot"
    );
    assert!(app.get_record(&corrupt_session).is_err());
    assert!(polling_page(&app, &owner, Some(&corrupt_session)).is_err());
    Ok(())
}

#[test]
fn legacy_deletion_uses_durable_validation_in_permission_view() -> TestResult {
    for valid in [true, false] {
        let root = Root::new()?;
        let app = application(&root, false)?;
        let session = session(&app, &root)?;
        let client = AuthenticatedClientId::new("client_legacy_view").ok_or("client")?;
        let owner = PermissionOwner {
            subject: "subject_view",
            client: &client,
        };
        let data = if valid {
            json!({"sessionID":session.as_str(),"info":{
                "id":session.as_str(),"slug":"legacy","projectID":"prj_view",
                "directory":root.0.to_str().ok_or("directory")?,"title":"view",
                "version":"1","time":{"created":100,"updated":100}}})
        } else {
            json!({"sessionID":session.as_str()})
        };
        {
            let mut inbox = app.lock_inbox();
            let aggregate = grapt_store::AggregateId::Session(session.clone());
            let writer = inbox.store().fencing_token(&aggregate)?;
            let tx = inbox.store_mut().governed_transaction()?;
            grapt_store::EventStore::append_within(
                &tx,
                grapt_store::AppendInput {
                    id: &"evt_legacy_view".parse()?,
                    aggregate,
                    writer,
                    kind_name: "session.deleted",
                    version: EventVersion::new(1).ok_or("version")?,
                    created_ms: 101,
                    data: &data,
                },
                &mut [],
            )?;
            tx.commit()?.discard();
        }
        assert!(app.get_record(&session).is_err());
        assert!(polling_page(&app, &owner, Some(&session)).is_err());
        if valid {
            assert!(
                matches!(
                    app.poll_committed_permissions(&owner, Some(&session)),
                    Some(Err(
                        crate::dogfood::permission_view::PollError::MissingSession(_)
                    ))
                ),
                "valid v1 deletion should leave an available snapshot without the Session"
            );
            parity(&app, &owner, None)?;
        } else {
            assert!(
                app.permission_view.allocation_bytes().is_none(),
                "malformed v1 deletion became a valid snapshot"
            );
        }
    }
    Ok(())
}

#[test]
fn four_hundred_sessions_have_a_bounded_measured_generation() -> TestResult {
    let root = Root::new()?;
    let app = application(&root, false)?;
    for _ in 0..400 {
        session(&app, &root)?;
    }
    let bytes = app
        .permission_view
        .allocation_bytes()
        .ok_or("400 Session snapshot unavailable")?;
    println!("permission view: 400 Sessions, allocation_bytes={bytes}, max_bytes=1048576");
    assert!(bytes <= 1_048_576);
    Ok(())
}

#[test]
fn row_and_byte_bounds_never_return_a_partial_permission_snapshot() -> TestResult {
    let root = Root::new()?;
    let app = application(&root, false)?;
    let session = session(&app, &root)?;
    let client = AuthenticatedClientId::new("client_view").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "subject_view",
        client: &client,
    };
    for index in 0..3 {
        ask(&app, &owner, &session, index, None)?;
    }
    let inbox = app.lock_inbox();
    assert!(grapt_permission::PermissionHttpSnapshot::read_bounded(
        inbox.store().connection(),
        2,
        1_048_576
    )?
    .is_none());
    assert!(grapt_permission::PermissionHttpSnapshot::read_bounded(
        inbox.store().connection(),
        3,
        1
    )?
    .is_none());
    let snapshot = grapt_permission::PermissionHttpSnapshot::read_bounded(
        inbox.store().connection(),
        3,
        1_048_576,
    )?
    .ok_or("exact row boundary refused")?;
    assert_eq!(snapshot.list(&owner, None)?.requests.len(), 3);
    Ok(())
}

#[test]
fn a_blocked_checkpoint_cannot_leave_permission_polling_on_a_pending_generation() -> TestResult {
    let root = Root::new()?;
    let app = application(&root, true)?;
    let session = session(&app, &root)?;
    let client = AuthenticatedClientId::new("client_barrier").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "subject_barrier",
        client: &client,
    };
    let id = ask(&app, &owner, &session, 950, None)?;
    let reader = rusqlite::Connection::open(root.0.join("database.sqlite3"))?;
    reader.execute_batch("BEGIN; SELECT * FROM permission_request")?;
    {
        let mut permissions = app.lock_permissions();
        let mut inbox = app.lock_inbox();
        inbox.store().connection().busy_timeout(Duration::ZERO)?;
        permissions.reply(
            inbox.store_mut(),
            grapt_permission::PermissionReplyCommand {
                owner: PermissionOwner {
                    subject: "subject_barrier",
                    client: &client,
                },
                session: &session,
                id: &id,
                reply: grapt_permission::PermissionHttpReply::Once,
                project: None,
                created_ms: 101,
                decision: app.mint_decision_ids()?,
            },
            || {
                Err(grapt_permission::PermissionHttpError::Replay(
                    "unexpected sibling reply".into(),
                ))
            },
        )?;
    }
    let stale: String = reader.query_row(
        "SELECT status FROM permission_request WHERE id=?1",
        [id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(
        stale, "pending",
        "the external reader still holds its old snapshot"
    );
    parity(&app, &owner, Some(&session))?;
    assert!(polling_page(&app, &owner, Some(&session))
        .map_err(|error| format!("{error:?}"))?
        .requests
        .is_empty());
    reader.execute_batch("COMMIT")?;
    Ok(())
}

#[test]
fn canonical_event_replay_rebuilds_every_http_coordinate_in_a_new_database() -> TestResult {
    let dir = std::env::temp_dir().join(format!(
        "grapt-permission-agent-replay-{}",
        std::process::id()
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let source_db = dir.join("source.db");
    let rebuilt_db = dir.join("rebuilt.db");
    let session: SessionId = "ses_agent_replay".parse()?;
    let id: PermissionId = "per_agent_replay".parse()?;
    let rule_id: PermissionId = "per_agent_rule".parse()?;
    let client = AuthenticatedClientId::new("client-agent-replay").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-agent-replay",
        client: &client,
    };
    let project = project()?;
    {
        let mut store = store_at(Some(&source_db), &session)?;
        let mut state = PermissionHttpState::default();
        assert_eq!(
            state.create(
                &mut store,
                &owner,
                &event("evt_agentrule")?,
                &rule_id,
                &session,
                &agent()?,
                &[],
                &project,
                "egress",
                &["api.example.test".to_owned()],
                Some(&["*.example.test".to_owned()]),
                None,
                None,
                31,
            )?,
            grapt_permission::PermissionCreateEffect::Ask
        );
        state.reply(
            &mut store,
            PermissionReplyCommand {
                owner: owner.clone(),
                session: &session,
                id: &rule_id,
                reply: PermissionHttpReply::Always,
                project: Some(&project),
                created_ms: 32,
                decision: decision("agentrule")?,
            },
            || Err(PermissionHttpError::Identity("no cascade".to_owned())),
        )?;
        let metadata = serde_json::json!({"reason": "replay witness"});
        let rules = PermissionHttpState::saved_rules(&store, Some(&project))?;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].project, project);
        assert_eq!(rules[0].action, "egress");
        assert_eq!(rules[0].resource, "*.example.test");
        assert!(PermissionHttpState::saved_rules(
            &store,
            Some(&ProjectId::new("prj_other").ok_or("project")?)
        )?
        .is_empty());
        let source = PermissionHttpSource::Tool {
            message_id: "msg-replay".to_owned(),
            call_id: "call-replay".to_owned(),
        };
        assert_eq!(
            state.create(
                &mut store,
                &owner,
                &event("evt_agentask")?,
                &id,
                &session,
                &agent()?,
                &[],
                &project,
                "write",
                &["/work/ask".to_owned()],
                Some(&["/work/*".to_owned()]),
                Some(&metadata),
                Some(&source),
                33,
            )?,
            grapt_permission::PermissionCreateEffect::Ask
        );
        let asked: (String, String, String, String) = store.connection().query_row(
            "SELECT type, json_extract(data, '$.agentID'),
                    json_extract(data, '$.ownerSubject'), json_extract(data, '$.ownerClient')
             FROM event WHERE id = 'evt_agentask'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(
            asked,
            (
                "permission.asked.2".to_owned(),
                "agt_http".to_owned(),
                "human-agent-replay".to_owned(),
                "client-agent-replay".to_owned()
            ),
            "canonical asked v2 must carry every owner and agent coordinate"
        );
    }

    let log = EventLog::open_read_only(&source_db)?;
    let mut store = store_at(Some(&rebuilt_db), &session)?;
    let before: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM permission_request", [], |row| {
                row.get(0)
            })?;
    assert_eq!(
        before, 0,
        "the counterexample destination must be a new database"
    );
    assert_eq!(
        PermissionHttpState::rebuild_from_event_log(&mut store, &log)?,
        3,
        "the rebuild must consume the complete canonical source log"
    );
    {
        let rebuilt = PermissionHttpState::default();
        let rows = rebuilt.list(&store, &owner, Some(&session))?;
        assert_eq!(
            rows.requests.len(),
            1,
            "fresh replay must restore adjunct request authority"
        );
        assert_eq!(rows.requests[0].id, id);
        assert_eq!(
            rows.requests[0].agent.as_str(),
            "agt_http",
            "fresh replay must retain the exact agent coordinate"
        );
        assert_eq!(rows.requests[0].resources, vec!["/work/ask"]);
        assert_eq!(rows.requests[0].save, Some(vec!["/work/*".to_owned()]));
        assert_eq!(
            rows.requests[0].metadata,
            Some(serde_json::json!({"reason": "replay witness"}))
        );
        assert_eq!(
            rows.requests[0].source,
            Some(PermissionHttpSource::Tool {
                message_id: "msg-replay".to_owned(),
                call_id: "call-replay".to_owned(),
            })
        );
    }
    let rebuilt = PermissionHttpState::default();
    assert_eq!(
        rebuilt.preview_tool(
            &store,
            &session,
            &[],
            &project,
            "egress",
            &["cdn.example.test".to_owned()],
            None,
        )?,
        grapt_permission::PermissionCreateEffect::Ask,
        "legacy wildcard text must not gain authority over another literal"
    );
    let export = grapt_store::export_canonical(store.connection())?;
    let replay_rows = |table: &str| {
        export
            .event_replayable
            .iter()
            .find(|digest| digest.table == table)
            .map(|digest| digest.rows)
    };
    assert_eq!(replay_rows("permission_http_request"), Some(2));
    assert_eq!(replay_rows("permission_http_rule"), Some(1));
    assert!(
        export
            .mutable_only
            .iter()
            .all(|digest| !digest.table.starts_with("permission_http_")),
        "replay-derived permission HTTP state must not remain classified mutable-only"
    );
    drop(store);
    drop(log);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn session_and_always_grants_reuse_only_matching_saved_resources() -> TestResult {
    let session: SessionId = "ses_grants".parse()?;
    let client = AuthenticatedClientId::new("client-grants").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-grants",
        client: &client,
    };
    let project = project()?;
    let mut store = store_at(None, &session)?;
    let mut state = PermissionHttpState::default();

    let session_request: PermissionId = "per_session_grant".parse()?;
    assert_eq!(
        state.create(
            &mut store,
            &owner,
            &event("evt_sessiongrant")?,
            &session_request,
            &session,
            &agent()?,
            &[],
            &project,
            "write",
            &["/work/one".to_owned()],
            Some(&["/work/one".to_owned()]),
            None,
            None,
            40,
        )?,
        grapt_permission::PermissionCreateEffect::Ask
    );
    assert_eq!(
        state.reply(
            &mut store,
            PermissionReplyCommand {
                owner: owner.clone(),
                session: &session,
                id: &session_request,
                reply: PermissionHttpReply::Session,
                project: Some(&project),
                created_ms: 41,
                decision: decision("sessiongrant")?,
            },
            || Err(PermissionHttpError::Identity("no cascade".to_owned())),
        )?,
        CasOutcome::Applied
    );
    let matching: PermissionId = "per_session_matching".parse()?;
    assert_eq!(
        state.create(
            &mut store,
            &owner,
            &event("evt_sessionmatch")?,
            &matching,
            &session,
            &agent()?,
            &[],
            &project,
            "write",
            &["/work/one".to_owned()],
            None,
            None,
            None,
            42,
        )?,
        grapt_permission::PermissionCreateEffect::Allow,
        "matching Session scope must reuse authority"
    );
    let nonmatching: PermissionId = "per_session_nonmatching".parse()?;
    assert_eq!(
        state.create(
            &mut store,
            &owner,
            &event("evt_sessionmiss")?,
            &nonmatching,
            &session,
            &agent()?,
            &[],
            &project,
            "write",
            &["/outside/two".to_owned()],
            None,
            None,
            None,
            43,
        )?,
        grapt_permission::PermissionCreateEffect::Ask,
        "a nonmatching resource must ask again"
    );

    assert_eq!(state.session_rules(&session)?.len(), 1);
    state.revoke_session_rule(&session, "write", "/work/one");
    assert!(state.session_rules(&session)?.is_empty());
    assert_eq!(
        state.create(
            &mut store,
            &owner,
            &event("evt_sessionwithdrawn")?,
            &"per_session_withdrawn".parse()?,
            &session,
            &agent()?,
            &[],
            &project,
            "write",
            &["/work/one".to_owned()],
            None,
            None,
            None,
            43,
        )?,
        grapt_permission::PermissionCreateEffect::Ask,
        "withdrawn session consent must ask again"
    );

    let always_request: PermissionId = "per_always_grant".parse()?;
    state.create(
        &mut store,
        &owner,
        &event("evt_alwaysgrant")?,
        &always_request,
        &session,
        &agent()?,
        &[],
        &project,
        "egress",
        &["api.example.test".to_owned()],
        Some(&["api.example.test".to_owned()]),
        None,
        None,
        44,
    )?;
    state.reply(
        &mut store,
        PermissionReplyCommand {
            owner: owner.clone(),
            session: &session,
            id: &always_request,
            reply: PermissionHttpReply::Always,
            project: Some(&project),
            created_ms: 45,
            decision: decision("alwaysgrant")?,
        },
        || Err(PermissionHttpError::Identity("no cascade".to_owned())),
    )?;
    let mut durable = PermissionHttpState::default();
    let durable_match: PermissionId = "per_always_matching".parse()?;
    assert_eq!(
        durable.create(
            &mut store,
            &owner,
            &event("evt_alwaysmatch")?,
            &durable_match,
            &session,
            &agent()?,
            &[],
            &project,
            "egress",
            &["api.example.test".to_owned()],
            None,
            None,
            None,
            46,
        )?,
        grapt_permission::PermissionCreateEffect::Allow,
        "Always scope must survive process-state reconstruction"
    );
    let durable_miss: PermissionId = "per_always_nonmatching".parse()?;
    assert_eq!(
        durable.create(
            &mut store,
            &owner,
            &event("evt_alwaysmiss")?,
            &durable_miss,
            &session,
            &agent()?,
            &[],
            &project,
            "egress",
            &["example.invalid".to_owned()],
            None,
            None,
            None,
            47,
        )?,
        grapt_permission::PermissionCreateEffect::Ask
    );
    Ok(())
}

#[test]
fn reject_cascade_keyset_batches_every_open_sibling_beyond_one_page() -> TestResult {
    let session: SessionId = "ses_cascade_pages".parse()?;
    let client = AuthenticatedClientId::new("client-cascade-pages").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-cascade-pages",
        client: &client,
    };
    let project = project()?;
    let mut store = store_at(None, &session)?;
    let mut state = PermissionHttpState::default();
    let primary: PermissionId = "per_cascade_primary".parse()?;
    state.create(
        &mut store,
        &owner,
        &event("evt_cascadeprimary")?,
        &primary,
        &session,
        &agent()?,
        &[],
        &project,
        "write",
        &[],
        None,
        None,
        None,
        50,
    )?;
    for index in 0..130usize {
        let id: PermissionId = format!("per_cascade{index:03}").parse()?;
        let event_id: EventId = format!("evt_cascade{index:03}").parse()?;
        state.create(
            &mut store,
            &owner,
            &event_id,
            &id,
            &session,
            &agent()?,
            &[],
            &project,
            "write",
            &[],
            None,
            None,
            None,
            50,
        )?;
    }
    let tx = store.governed_transaction()?;
    tx.execute(
        "UPDATE permission_request SET status = 'suspended', suspended_reason = 'shutdown'
         WHERE id IN (SELECT id FROM permission_request WHERE id != ?1 ORDER BY id LIMIT 65)",
        [primary.as_str()],
    )?;
    tx.commit()?.discard();
    let mut minted = 0usize;
    state.reply(
        &mut store,
        PermissionReplyCommand {
            owner: owner.clone(),
            session: &session,
            id: &primary,
            reply: PermissionHttpReply::Reject,
            project: Some(&project),
            created_ms: 51,
            decision: decision("cascadeprimary")?,
        },
        || {
            let current = minted;
            minted = minted.saturating_add(1);
            decision(&format!("cascade{current:03}"))
                .map_err(|error| PermissionHttpError::Identity(error.to_string()))
        },
    )?;
    assert_eq!(
        minted, 130,
        "every pending and suspended sibling must settle"
    );
    let remaining: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM permission_request WHERE status IN ('pending', 'suspended')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        remaining, 0,
        "no residual open sibling may survive page one"
    );
    Ok(())
}

#[test]
fn open_list_reports_truncation_and_get_does_not_inherit_its_page_limit() -> TestResult {
    let session: SessionId = "ses_list_pages".parse()?;
    let client = AuthenticatedClientId::new("client-list-pages").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "human-list-pages",
        client: &client,
    };
    let project = project()?;
    let mut store = store_at(None, &session)?;
    let mut state = PermissionHttpState::default();
    let oldest: PermissionId = "per_page000".parse()?;
    for index in 0..129usize {
        let id: PermissionId = format!("per_page{index:03}").parse()?;
        let event_id: EventId = format!("evt_page{index:03}").parse()?;
        state.create(
            &mut store,
            &owner,
            &event_id,
            &id,
            &session,
            &agent()?,
            &[],
            &project,
            "read",
            &[],
            None,
            None,
            None,
            60,
        )?;
    }
    let page = state.list(&store, &owner, Some(&session))?;
    assert_eq!(page.requests.len(), 128);
    assert!(
        page.has_more,
        "a bounded list must disclose its omitted live row"
    );
    let fetched = match state.get(&store, &owner, &session, &oldest) {
        Ok(request) => request,
        Err(error) => {
            panic!("get must query its identity directly instead of searching page one: {error}")
        }
    };
    assert_eq!(
        fetched.id, oldest,
        "get must query its identity directly instead of searching page one"
    );
    state.reply(
        &mut store,
        PermissionReplyCommand {
            owner: owner.clone(),
            session: &session,
            id: &oldest,
            reply: PermissionHttpReply::Once,
            project: Some(&project),
            created_ms: 61,
            decision: decision("pageoldest")?,
        },
        || Err(PermissionHttpError::Identity("no cascade".to_owned())),
    )?;
    assert!(matches!(
        state.get(&store, &owner, &session, &oldest),
        Err(PermissionHttpError::NotFound(found)) if found == oldest
    ));
    let after = state.list(&store, &owner, Some(&session))?;
    assert_eq!(after.requests.len(), 128);
    assert!(
        !after.has_more,
        "terminal rows are excluded from the open list"
    );
    Ok(())
}

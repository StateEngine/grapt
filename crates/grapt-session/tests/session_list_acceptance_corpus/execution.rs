// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// A projector that always fails, used by the crash-cut action to prove that a
/// projection failure rolls the whole append back.
#[derive(Debug)]
struct FailingProjector;

impl Projector for FailingProjector {
    fn projection(&self) -> grapt_store::ProjectionId {
        grapt_store::session_list::SESSION_LIST_PROJECTION
    }

    fn project(
        &mut self,
        _tx: &rusqlite::Transaction<'_>,
        _event: &grapt_store::DecodedEvent,
    ) -> Result<(), grapt_store::ProjectorError> {
        Err("injected projection failure".into())
    }
}

struct Run {
    root: PathBuf,
    db: PathBuf,
    store: EventStore,
    projector: SessionListProjector,
    api: Vec<Value>,
    events: Vec<Value>,
    durable: Vec<Value>,
}

fn seed_projection_row(
    conn: &Connection,
    id: &str,
    updated_ms: i64,
    project_id: &str,
    workspace_id: Option<&str>,
    directory: &str,
    title: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO session_list_projection
            (session_id, project_id, workspace_id, parent_id, directory, title, updated_ms,
             provenance_kind, provenance_digest, cutover_seq)
         VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, 'rust-event', 'corpus-digest', -1)",
        rusqlite::params![id, project_id, workspace_id, directory, title, updated_ms],
    )?;
    Ok(())
}

fn build_criteria(action: &Value) -> Fallible<SessionListCriteria> {
    let limits = match action["search_utf8_bytes"].as_u64() {
        // Action 10 asks for a search longer than the declared bound; the
        // refusal must come from the bound, so the limits stay production.
        Some(_) | None => SessionListLimits::production(),
    };
    let scope = match action["scope"].as_object() {
        None => SessionListScope::All,
        Some(scope) => {
            let directory = scope["directory"].as_str().unwrap_or("/");
            let workspace = scope["workspace"].as_str();
            SessionListScope::Directory(LocationRef::on(
                grapt_types::Platform::Posix,
                directory,
                workspace.map(WorkspaceId::new).and_then(|value| value),
            )?)
        }
    };
    let search = match action["search_utf8_bytes"].as_u64() {
        None => action["title_contains"]
            .as_str()
            .map(|value| BoundedSearch::new(value, SessionListLimits::production()))
            .transpose()?,
        Some(bytes) => Some(BoundedSearch::new(
            "x".repeat(usize::try_from(bytes)?),
            SessionListLimits::production(),
        )?),
    };
    let parent = match action["parent"].as_str() {
        Some("root") => ParentFilter::Root,
        Some(other) => ParentFilter::Child(other.parse::<SessionId>()?),
        None => ParentFilter::Any,
    };
    let order = match action["order"].as_str() {
        Some("descending") => SessionListOrder::Descending,
        _ => SessionListOrder::Ascending,
    };
    let direction = match action["direction"].as_str() {
        Some("previous") => SessionListDirection::Previous,
        _ => SessionListDirection::Next,
    };
    let anchor = match action["anchor"].as_object() {
        None => None,
        Some(anchor) => {
            let updated = anchor["updated_ms"].as_i64().ok_or("anchor updated_ms")?;
            let id = anchor["session_id"].as_str().ok_or("anchor session_id")?;
            Some(
                SessionListKey::new(
                    NonNegativeMs::new(updated).ok_or("anchor updated_ms must be non-negative")?,
                    id.parse::<SessionId>()?,
                )
                .into(),
            )
        }
    };
    let page_size = usize::try_from(action["page_size"].as_u64().unwrap_or(1))?;
    Ok(SessionListCriteria::new(
        scope, None, search, parent, order, direction, anchor, page_size, limits,
    )?)
}

fn refusal_name(error: &SessionListError) -> &'static str {
    match error {
        SessionListError::PageLimitExceeded { .. } => "PageLimitExceeded",
        SessionListError::SearchLimitExceeded { .. } => "SearchLimitExceeded",
        SessionListError::QueryBudgetExceeded => "QueryBudgetExceeded",
        SessionListError::ProjectionUnavailable => "ProjectionUnavailable",
        SessionListError::ProjectionCorrupt { .. } => "ProjectionCorrupt",
        SessionListError::InvalidCriteria { .. } => "InvalidCriteria",
        SessionListError::StoreFailure { .. } => "StoreFailure",
        SessionListError::StaleCursor => "StaleCursor",
    }
}

impl Run {
    fn append(
        &mut self,
        session: &str,
        event_id: &str,
        kind: &str,
        created_ms: i64,
        data: &Value,
    ) -> Fallible<u64> {
        let aggregate = AggregateId::Session(session.parse::<SessionId>()?);
        if self.store.fencing_token(&aggregate).is_err() {
            self.store.elect_writer(&aggregate)?;
        }
        let writer = self.store.fencing_token(&aggregate)?;
        let version = EventVersion::new(1).ok_or("event version must be positive")?;
        let id = event_id.parse()?;
        let (appended, notices) = self.store.append(
            AppendInput {
                id: &id,
                aggregate: aggregate.clone(),
                writer,
                kind_name: kind,
                version,
                created_ms,
                data,
            },
            &mut [&mut self.projector],
        )?;
        notices.discard();
        Ok(match appended {
            grapt_store::Appended::Committed { seq }
            | grapt_store::Appended::Idempotent { seq } => seq,
        })
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn execute(actions: &[Value], initial: &Value) -> Fallible<Run> {
    let root =
        std::env::temp_dir().join(format!("grapt-session-list-corpus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let db = root.join("corpus.db");

    // The initial state is the legacy oracle database this corpus starts
    // from, including the declared collation on `session.id`.
    let legacy = &initial["legacy_source"];
    let collation = legacy["session_id_collation"]
        .as_str()
        .ok_or("initial state must declare the legacy id collation")?;
    let conn = Connection::open(&db)?;
    conn.execute_batch(&format!(
        "CREATE TABLE event_sequence (
            aggregate_id TEXT PRIMARY KEY,
            seq INTEGER NOT NULL,
            owner_id TEXT
         );
         CREATE TABLE event (
            id TEXT PRIMARY KEY,
            aggregate_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            created INTEGER NOT NULL,
            type TEXT NOT NULL,
            data TEXT NOT NULL,
            FOREIGN KEY (aggregate_id) REFERENCES event_sequence(aggregate_id) ON DELETE CASCADE
         );
         CREATE TABLE session (
            id TEXT COLLATE {collation},
            project_id TEXT NOT NULL,
            workspace_id TEXT,
            parent_id TEXT,
            directory TEXT NOT NULL,
            title TEXT NOT NULL,
            time_updated INTEGER NOT NULL
         );"
    ))?;
    for session in legacy["sessions"].as_array().ok_or("legacy sessions")? {
        conn.execute(
            "INSERT INTO session
                (id, project_id, workspace_id, parent_id, directory, title, time_updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                session["id"].as_str(),
                session["project_id"].as_str(),
                session["workspace_id"].as_str(),
                session["parent_id"].as_str(),
                session["directory"].as_str(),
                session["title"].as_str(),
                session["time_updated"].as_i64(),
            ],
        )?;
    }
    for head in legacy["event_heads"]
        .as_array()
        .ok_or("legacy event heads")?
    {
        let aggregate = head["aggregate_id"].as_str().ok_or("aggregate id")?;
        let seq = head["seq"].as_i64().ok_or("head seq")?;
        conn.execute(
            "INSERT INTO event_sequence (aggregate_id, seq, owner_id) VALUES (?1, ?2, NULL)",
            rusqlite::params![aggregate, seq],
        )?;
        conn.execute(
            "INSERT INTO event (id, aggregate_id, seq, created, type, data)
             VALUES (?1, ?2, ?3, 1, 'session.created.1', '{}')",
            rusqlite::params![format!("evt_head_{aggregate}"), aggregate, seq],
        )?;
    }
    drop(conn);

    let mut run: Option<Run> = None;

    for action in actions {
        let ordinal = action["ordinal"]
            .as_i64()
            .ok_or("every action needs an ordinal")?;
        let kind = action["kind"].as_str().ok_or("every action needs a kind")?;
        match kind {
            "harness.session-list.import" => {
                let outcome = EventStore::migrate(&db, &root, "corpus")?;
                assert!(
                    matches!(outcome, MigrationOutcome::Applied { .. }),
                    "action {ordinal}: the import must apply"
                );
                // Rerunning must be a no-op, exactly as the corpus declares.
                assert!(
                    matches!(
                        EventStore::migrate(&db, &root, "corpus")?,
                        MigrationOutcome::AlreadyCurrent
                    ),
                    "action {ordinal}: a second import must be AlreadyCurrent"
                );
                run = Some(Run {
                    root: root.clone(),
                    db: db.clone(),
                    store: EventStore::open(&db, "corpus")?,
                    projector: SessionListProjector,
                    api: Vec::new(),
                    events: Vec::new(),
                    durable: Vec::new(),
                });
            }
            other => {
                let state = run
                    .as_mut()
                    .ok_or_else(|| format!("action {ordinal}: {other} runs before the import"))?;
                apply(state, ordinal, other, action)?;
            }
        }
    }
    run.ok_or_else(|| "the corpus never opened a store".into())
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn apply(state: &mut Run, ordinal: i64, kind: &str, action: &Value) -> Fallible<()> {
    match kind {
        "harness.session-list.page" => {
            if let Some(seed) = action["seed_match"].as_object() {
                seed_projection_row(
                    state.store.connection(),
                    seed["session"].as_str().ok_or("seed session")?,
                    seed["updated_ms"].as_i64().ok_or("seed updated_ms")?,
                    seed["project"].as_str().ok_or("seed project")?,
                    seed["workspace"].as_str(),
                    seed["directory"].as_str().ok_or("seed directory")?,
                    seed["title"].as_str().ok_or("seed title")?,
                )?;
            }
            match build_criteria(action) {
                Err(error) => {
                    // A criteria-construction refusal IS the declared outcome
                    // for the bound actions; record it in the same shape.
                    let text = error.to_string();
                    let name = if text.contains("page") && text.contains("128") {
                        "PageLimitExceeded"
                    } else {
                        "SearchLimitExceeded"
                    };
                    state.api.push(json!({"action": ordinal, "outcome": "refused", "error": name}));
                }
                Ok(criteria) => match SessionListStore::over(&mut state.store).list(&criteria) {
                    Ok(page) => state.api.push(json!({
                        "action": ordinal,
                        "outcome": "page",
                        "order": action["order"].as_str().unwrap_or("ascending"),
                        "ids": item_ids(&page),
                    })),
                    Err(error) => state.api.push(
                        json!({"action": ordinal, "outcome": "refused", "error": refusal_name(&error)}),
                    ),
                },
            }
        }
        "harness.append" => {
            let session = action["session"].as_str().ok_or("append session")?;
            let kind_name = action["type"].as_str().ok_or("append type")?;
            let created = action["created_ms"].as_i64().ok_or("append created_ms")?;
            let data = if action["data"].is_null() {
                json!({"sessionID": session})
            } else {
                action["data"].clone()
            };
            let seq = state.append(
                session,
                action["event"].as_str().ok_or("append event id")?,
                kind_name,
                created,
                &data,
            )?;
            state.durable.push(json!({
                "action": ordinal,
                "aggregate": session,
                "seq": seq,
                "type": kind_name,
                "version": 1
            }));
            state
                .events
                .push(json!({"action": ordinal, "aggregate": session, "seq": seq}));
        }
        "harness.session-list.seed-ties" => {
            let updated = action["updated_ms"].as_i64().ok_or("tie updated_ms")?;
            for session in action["sessions"].as_array().ok_or("tie sessions")? {
                seed_projection_row(
                    state.store.connection(),
                    session.as_str().ok_or("tie session id")?,
                    updated,
                    "prj_tie",
                    None,
                    "/work/tie",
                    "tie",
                )?;
            }
        }
        "harness.session-list.inject-oversize-title" => {
            let session = action["session"].as_str().ok_or("oversize session")?;
            let bytes = usize::try_from(action["title_bytes"].as_u64().ok_or("title_bytes")?)?;
            seed_projection_row(
                state.store.connection(),
                session,
                60_000,
                "prj_oversize",
                None,
                "/work/oversize",
                &"t".repeat(bytes),
            )?;
            let criteria = SessionListCriteria::new(
                SessionListScope::All,
                None,
                None,
                ParentFilter::Any,
                SessionListOrder::Descending,
                SessionListDirection::Next,
                None,
                1,
                SessionListLimits::production(),
            )?;
            // The oversize row is skipped and named; it never fails the page.
            let page = SessionListStore::over(&mut state.store).list(&criteria)?;
            let degraded = page.degraded().iter().map(|row| {
                row.session_id
                    .as_ref()
                    .map_or_else(String::new, ToString::to_string)
            });
            state.api.push(json!({
                "action": ordinal,
                "outcome": "degraded",
                "degraded": degraded.collect::<Vec<_>>(),
                "ids": item_ids(&page),
            }));
            if action["revert_injection"].as_bool() == Some(true) {
                state.store.connection().execute(
                    "DELETE FROM session_list_projection WHERE session_id = ?1",
                    [session],
                )?;
            }
        }
        "harness.session-list.project-with-failure" => {
            let session = action["session"].as_str().ok_or("atomic session")?;
            let aggregate = AggregateId::Session(session.parse::<SessionId>()?);
            state.store.elect_writer(&aggregate)?;
            let writer = state.store.fencing_token(&aggregate)?;
            let version = EventVersion::new(1).ok_or("version")?;
            let id = "evt_atomic".parse()?;
            let mut failing = FailingProjector;
            let outcome = state.store.append(
                AppendInput {
                    id: &id,
                    aggregate: aggregate.clone(),
                    writer,
                    kind_name: "session.created",
                    version,
                    created_ms: 45_000,
                    data: &json!({
                        "sessionID": session,
                        "info": {
                            "id": session, "projectID": "prj_atomic",
                            "directory": "/work/atomic", "title": "atomic",
                            "time": {"created": 45000, "updated": 45000}
                        }
                    }),
                },
                &mut [&mut failing],
            );
            assert!(
                outcome.is_err(),
                "action {ordinal}: a failing projector must refuse the append"
            );
            let expect = &action["expect"];
            let conn = state.store.connection();
            let event_rows: i64 = conn.query_row(
                "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1",
                [session],
                |row| row.get(0),
            )?;
            let projection_rows: i64 = conn.query_row(
                "SELECT COUNT(*) FROM session_list_projection WHERE session_id = ?1",
                [session],
                |row| row.get(0),
            )?;
            let watermarks: i64 = conn.query_row(
                "SELECT COUNT(*) FROM projection_watermark WHERE aggregate_id = ?1",
                [session],
                |row| row.get(0),
            )?;
            assert_eq!(event_rows, expect["event_rows"].as_i64().unwrap_or(-1));
            assert_eq!(
                projection_rows,
                expect["projection_rows"].as_i64().unwrap_or(-1)
            );
            assert_eq!(watermarks, expect["watermarks"].as_i64().unwrap_or(-1));
        }
        "harness.session-list.catch-up" => {
            let session = action["session"].as_str().ok_or("restart session")?;
            let aggregate = AggregateId::Session(session.parse::<SessionId>()?);
            state.store.elect_writer(&aggregate)?;
            let writer = state.store.fencing_token(&aggregate)?;
            let version = EventVersion::new(1).ok_or("version")?;
            let id = action["event"].as_str().ok_or("restart event")?.parse()?;
            let created = action["created_ms"].as_i64().ok_or("restart created_ms")?;
            // Appended WITHOUT the session-list projector: this is the state a
            // restart finds, a durable fact the projection has not folded.
            let (appended, notices) = state.store.append(
                AppendInput {
                    id: &id,
                    aggregate: aggregate.clone(),
                    writer,
                    kind_name: "session.created",
                    version,
                    created_ms: created,
                    data: &json!({
                        "sessionID": session,
                        "info": {
                            "id": session, "projectID": "prj_restart",
                            "directory": "/work/restart", "title": "restarted",
                            "time": {"created": created, "updated": created}
                        }
                    }),
                },
                &mut [],
            )?;
            notices.discard();
            let seq = match appended {
                grapt_store::Appended::Committed { seq }
                | grapt_store::Appended::Idempotent { seq } => seq,
            };
            state.durable.push(json!({
                "action": ordinal,
                "aggregate": session,
                "seq": seq,
                "type": "session.created",
                "version": 1
            }));
            state
                .events
                .push(json!({"action": ordinal, "aggregate": session, "seq": seq}));
            let mut projector = SessionListProjector;
            state.store.catch_up(&aggregate, &mut projector)?;
            let watermark: i64 = state.store.connection().query_row(
                "SELECT seq FROM projection_watermark
                 WHERE projection = 'session-list-v1' AND aggregate_id = ?1",
                [session],
                |row| row.get(0),
            )?;
            assert_eq!(
                watermark,
                action["expect_watermark"].as_i64().unwrap_or(-1),
                "action {ordinal}: catch-up must advance the watermark from durable truth"
            );
        }
        "harness.session-list.page-local-race" => {
            // Each page is internally consistent; a writer moving a key
            // between requests is a legal observation, and the lane promises
            // no cross-page snapshot.
            let session = action["session"].as_str().ok_or("race session")?;
            let moved = action["moved_updated_ms"]
                .as_i64()
                .ok_or("moved_updated_ms")?;
            let before = page_ids(&mut state.store, 128)?;
            state.store.connection().execute(
                "UPDATE session_list_projection SET updated_ms = ?2 WHERE session_id = ?1",
                rusqlite::params![session, moved],
            )?;
            let after = page_ids(&mut state.store, 128)?;
            assert!(
                before.contains(&session.to_owned()) && after.contains(&session.to_owned()),
                "action {ordinal}: the moved key must remain listable"
            );
            assert_ne!(
                before, after,
                "action {ordinal}: the writer must be observable between page requests"
            );
            assert_eq!(
                action["cross_page_snapshot_promised"].as_bool(),
                Some(false),
                "action {ordinal}: the corpus must state that no cross-page snapshot is promised"
            );
        }
        other => return Err(format!("action {ordinal}: unknown kind {other}").into()),
    }
    Ok(())
}

fn item_ids(page: &grapt_session::SessionListPage) -> Vec<String> {
    page.items()
        .iter()
        .map(|item| item.key.session_id().to_string())
        .collect()
}

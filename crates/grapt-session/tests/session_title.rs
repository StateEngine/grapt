use grapt_session::title::{name_from_first_prompt, prompt_title, rename_session, DEFAULT_TITLE};
use grapt_session::{create_session, SessionListProjector, SessionSeed, SessionStore};
use grapt_store::{AggregateId, AppendInput, EventStore};
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, EventVersion, IdAuthority, IdToken, SessionId,
    Timestamp,
};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type TestResult = Result<(), Box<dyn std::error::Error>>;
#[derive(Debug, Default)]
struct Sources(AtomicU64);
impl ClockAuthority for Sources {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1234)
    }
}
impl IdAuthority for Sources {
    fn mint(&self) -> IdToken {
        IdToken::new(format!("title{}", self.0.fetch_add(1, Ordering::SeqCst)))
            .unwrap_or_else(|| panic!("fixture identity refused"))
    }
}
fn create(
    store: &mut EventStore,
    source: &Sources,
    parent: Option<SessionId>,
) -> Result<SessionId, Box<dyn std::error::Error>> {
    let mut seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_title").ok_or("fixture project ID refused")?,
        "/work/title",
        DEFAULT_TITLE,
    )?;
    if let Some(parent) = parent {
        seed = seed.with_parent(parent);
    }
    Ok(create_session(
        store,
        &DeterministicAuthorities::new(source, source),
        &seed,
        &mut [&mut SessionListProjector],
    )?
    .session)
}
fn admit(store: &mut EventStore, session: &SessionId, text: &str, number: u64) -> TestResult {
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let id = format!("evt_admit{number}").parse()?;
    let (_, notices) = store.append(AppendInput {
        id: &id, aggregate, writer, kind_name:"session.input.admitted",
        version: EventVersion::new(1).ok_or("version refused")?, created_ms:1234,
        data: &json!({"sessionID":session.as_str(),"inputID":format!("msg_input{number}"),"input":{"type":"user","data":{"text":text},"delivery":"queue"}}),
    }, &mut [])?;
    notices.discard();
    Ok(())
}
fn rename_count(store: &EventStore, session: &SessionId) -> rusqlite::Result<i64> {
    store.connection().query_row(
        "SELECT count(*) FROM event WHERE aggregate_id=?1 AND type='session.renamed.1'",
        [session.as_str()],
        |row| row.get(0),
    )
}
#[test]
fn title_is_a_printable_bounded_first_nonempty_line() {
    assert_eq!(
        prompt_title("\n \t\n Fix  日本語\tquantity\u{0} \nsecond"),
        Some("Fix 日本語 quantity".into())
    );
    let long = prompt_title(&"🦀".repeat(101)).unwrap_or_else(|| panic!("nonempty title missing"));
    assert_eq!(long.chars().count(), 100);
    assert!(long.ends_with("..."));
    assert_eq!(prompt_title(" \n\u{0}\t"), None);
}
#[test]
fn crash_between_admission_and_name_retries_the_original_prompt() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let source = Sources::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let session = create(&mut store, &source, None)?;
    assert_eq!(
        name_from_first_prompt(&mut store, &authorities, &session)?,
        None
    );
    admit(&mut store, &session, "Fix quantity calculation\nDetails", 1)?;
    // Stop at the admitted-but-untitled crash boundary. The next admission
    // retries naming from the original canonical input, not the new request.
    assert_eq!(
        SessionStore::over(&store).record(&session)?.title,
        DEFAULT_TITLE
    );
    admit(&mut store, &session, "Second prompt should not name it", 2)?;
    assert!(name_from_first_prompt(&mut store, &authorities, &session)?.is_some());
    assert_eq!(
        SessionStore::over(&store).record(&session)?.title,
        "Fix quantity calculation"
    );
    assert_eq!(
        name_from_first_prompt(&mut store, &authorities, &session)?,
        None
    );
    assert_eq!(rename_count(&store, &session)?, 1);
    let projected: String = store.connection().query_row(
        "SELECT title FROM session_list_projection WHERE session_id=?1",
        [session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(projected, "Fix quantity calculation");
    Ok(())
}
#[test]
fn a_manual_name_including_the_default_is_never_overwritten() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let source = Sources::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    for (number, title) in [(3, "My release task"), (4, DEFAULT_TITLE)] {
        let session = create(&mut store, &source, None)?;
        assert!(rename_session(&mut store, &authorities, &session, title)?.is_some());
        admit(
            &mut store,
            &session,
            "Automatic name must not replace this",
            number,
        )?;
        assert_eq!(
            name_from_first_prompt(&mut store, &authorities, &session)?,
            None,
            "MANUAL_TITLE_OVERWRITTEN"
        );
        assert_eq!(
            SessionStore::over(&store).record(&session)?.title,
            title,
            "MANUAL_TITLE_OVERWRITTEN"
        );
        assert_eq!(rename_count(&store, &session)?, 1);
    }
    Ok(())
}
#[test]
fn children_and_sessions_without_human_admission_keep_their_name() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let source = Sources::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let root = create(&mut store, &source, None)?;
    let child = create(&mut store, &source, Some(root.clone()))?;
    admit(&mut store, &child, "Synthetic child task", 5)?;
    assert_eq!(
        name_from_first_prompt(&mut store, &authorities, &child)?,
        None
    );
    // A root can receive an authorized synthetic input before any human.
    let aggregate = AggregateId::Session(root.clone());
    let writer = store.fencing_token(&aggregate)?;
    let (_,notices)=store.append(AppendInput {
        id:&"evt_synthetic_title".parse()?, aggregate, writer,
        kind_name:"session.input.admitted", version:EventVersion::new(1).ok_or("version refused")?, created_ms:1234,
        data:&json!({"sessionID":root.as_str(),"inputID":"msg_synthetic_title","input":{"type":"synthetic","rustAuthorized":true,"data":{"text":"Synthetic root task","description":"fixture"},"delivery":"queue"}}),
    },&mut [])?;
    notices.discard();
    assert_eq!(
        name_from_first_prompt(&mut store, &authorities, &root)?,
        None
    );
    let aggregate = AggregateId::Session(root.clone());
    let writer = store.fencing_token(&aggregate)?;
    let (_, notices) = store.append(AppendInput {
        id: &"evt_startup_title".parse()?, aggregate, writer,
        kind_name: "session.input.admitted", version: EventVersion::new(1).ok_or("version refused")?, created_ms: 1234,
        data: &json!({"sessionID":root.as_str(),"inputID":"msg_startup_title","input":{"type":"user","data":{"text":"PRIVATE_SEAT_PACKET","metadata":{"origin":"seat-startup"}},"delivery":"queue"}}),
    }, &mut [])?;
    notices.discard();
    assert_eq!(
        name_from_first_prompt(&mut store, &authorities, &root)?,
        None
    );
    assert_eq!(
        SessionStore::over(&store).record(&root)?.title,
        DEFAULT_TITLE
    );
    admit(&mut store, &root, "A human root task", 15)?;
    assert!(name_from_first_prompt(&mut store, &authorities, &root)?.is_some());
    assert_eq!(
        SessionStore::over(&store).record(&root)?.title,
        "A human root task"
    );
    assert_eq!(rename_count(&store, &child)?, 0);
    Ok(())
}
#[test]
fn invalid_manual_names_have_no_effect() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let source = Sources::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let session = create(&mut store, &source, None)?;
    for invalid in [
        String::new(),
        "\t".into(),
        "two\nlines".into(),
        "a".repeat(101),
    ] {
        assert!(rename_session(&mut store, &authorities, &session, &invalid).is_err());
    }
    assert_eq!(rename_count(&store, &session)?, 0);
    Ok(())
}
#[test]
fn concurrent_first_prompts_commit_exactly_one_name() -> TestResult {
    let source = Arc::new(Sources::default());
    let mut store = EventStore::open_in_memory()?;
    let session = create(&mut store, &source, None)?;
    let store = Arc::new(Mutex::new(store));
    let mut workers = Vec::new();
    for number in 6..10 {
        let source = Arc::clone(&source);
        let store = Arc::clone(&store);
        let session = session.clone();
        workers.push(std::thread::spawn(move || -> Result<(), String> {
            {
                let mut held = store.lock().map_err(|error| error.to_string())?;
                admit(&mut held, &session, &format!("Task {number}"), number)
                    .map_err(|error| error.to_string())?;
            }
            let mut held = store.lock().map_err(|error| error.to_string())?;
            name_from_first_prompt(
                &mut held,
                &DeterministicAuthorities::new(&*source, &*source),
                &session,
            )
            .map_err(|error| error.to_string())?;
            Ok(())
        }));
    }
    for worker in workers {
        worker.join().map_err(|_| "title worker panicked")??;
    }
    let held = store.lock().map_err(|_| "title store poisoned")?;
    assert_eq!(rename_count(&held, &session)?, 1);
    assert!(SessionStore::over(&held)
        .record(&session)?
        .title
        .starts_with("Task "));
    Ok(())
}

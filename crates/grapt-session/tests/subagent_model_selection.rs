use std::cell::Cell;

use grapt_session::selection::{select_subagent_model, subagent_model};
use grapt_session::{create_session, SessionSeed, SessionStoreError};
use grapt_store::{AggregateId, AppendInput, EventStore};
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, EventVersion, IdAuthority, IdToken, Timestamp,
};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Default)]
struct Authorities(Cell<u64>);
impl ClockAuthority for Authorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1234)
    }
}
impl IdAuthority for Authorities {
    fn mint(&self) -> IdToken {
        self.0.set(self.0.get() + 1);
        IdToken::new(format!("subagent{}", self.0.get()))
            .unwrap_or_else(|| panic!("nonempty fixture identity refused"))
    }
}

#[test]
fn subagent_preference_survives_reopen_and_clear_cannot_resurrect_selection() -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "grapt-subagent-selection-{}.sqlite",
        std::process::id()
    ));
    assert!(
        !path.exists(),
        "refusing to overwrite an existing witness database"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "subagent-selection-witness")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_subagent").ok_or("fixture project ID refused")?,
        "/work/subagent",
        "subagent preference",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    assert_eq!(subagent_model(store.connection(), &session)?, None);
    assert!(matches!(
        subagent_model(store.connection(), &"ses_missing".parse()?),
        Err(SessionStoreError::Unknown { .. })
    ));
    let model = json!({"providerID":"openai", "id":"model-a"});
    let first = select_subagent_model(&mut store, &authorities, &session, Some(&model))?;
    assert!(first.is_some());
    assert_eq!(
        select_subagent_model(
            &mut store,
            &authorities,
            &session,
            Some(&json!({"providerID":"openai", "id":"model-a", "variant":"default"}))
        )?,
        None
    );
    drop(store);
    let mut store = EventStore::open(&path, "subagent-selection-witness")?;
    assert_eq!(subagent_model(store.connection(), &session)?, Some(model));
    assert!(select_subagent_model(&mut store, &authorities, &session, None)?.is_some());
    assert_eq!(
        select_subagent_model(&mut store, &authorities, &session, None)?,
        None
    );
    drop(store);
    let store = EventStore::open(&path, "subagent-selection-witness")?;
    assert_eq!(
        subagent_model(store.connection(), &session)?,
        None,
        "cleared model resurrected after reopen"
    );
    drop(store);
    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn malformed_latest_preference_refuses_read_and_clear_instead_of_becoming_default() -> TestResult {
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory()?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_subagent").ok_or("fixture project ID refused")?,
        "/work/subagent",
        "corruption",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let (_, notices) = store.append(
        AppendInput {
            id: &"evt_malformed_model".parse()?,
            aggregate,
            writer,
            kind_name: "session.subagent-model.selected",
            version: EventVersion::new(1).ok_or("nonzero version refused")?,
            created_ms: 1235,
            data: &json!({"sessionID":session.as_str(), "model":{"id":"missing-provider"}}),
        },
        &mut [],
    )?;
    notices.discard();
    assert!(matches!(
        subagent_model(store.connection(), &session),
        Err(SessionStoreError::Corrupt { .. })
    ));
    assert!(select_subagent_model(&mut store, &authorities, &session, None).is_err());
    Ok(())
}

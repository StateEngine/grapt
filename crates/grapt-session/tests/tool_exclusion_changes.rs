//! Exclusion changes remain observable beyond the named notice prefix.
use std::cell::Cell;

use grapt_session::notice::{read_notices, record_tool_excluded, ExcludedTool};
use grapt_session::{create_session, SessionSeed};
use grapt_store::EventStore;
use grapt_types::{ClockAuthority, DeterministicAuthorities, IdAuthority, IdToken, Timestamp};

#[derive(Debug, Default)]
struct Authorities(Cell<u64>);
impl ClockAuthority for Authorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(100)
    }
}
impl IdAuthority for Authorities {
    fn mint(&self) -> IdToken {
        self.0.set(self.0.get() + 1);
        IdToken::new(format!("exclusion{}", self.0.get()))
            .unwrap_or_else(|| panic!("fixture identity refused"))
    }
}

fn fixture() -> Result<(EventStore, Authorities, grapt_types::SessionId), Box<dyn std::error::Error>>
{
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open_in_memory_with_ids(&source)?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_exclusion").ok_or("project identity")?,
        "/work/exclusion",
        "exclusion",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    Ok((store, source, session))
}

fn exclusions(tail: &str) -> Vec<ExcludedTool> {
    let mut tools = (0..64)
        .map(|index| ExcludedTool {
            name: format!("tool_{index:03}"),
            reason: "description is too long".into(),
        })
        .collect::<Vec<_>>();
    tools.push(ExcludedTool {
        name: tail.into(),
        reason: "malformed schema".into(),
    });
    tools
}

#[test]
fn replacing_one_omitted_tool_records_a_changed_exclusion_set(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut store, source, session) = fixture()?;
    let authorities = DeterministicAuthorities::new(&source, &source);
    record_tool_excluded(&mut store, &authorities, &session, exclusions("z_old"))?
        .ok_or("initial exclusion notice absent")?;
    let changed = record_tool_excluded(&mut store, &authorities, &session, exclusions("z_new"))?;
    assert!(
        changed.is_some(),
        "a different omitted tool was treated as unchanged"
    );
    let notices = read_notices(store.connection(), &session)?.tool_excluded;
    assert_eq!(notices.len(), 2);
    Ok(())
}

#[test]
fn colliding_display_names_count_every_original_exclusion() -> Result<(), Box<dyn std::error::Error>>
{
    let (mut store, source, session) = fixture()?;
    let authorities = DeterministicAuthorities::new(&source, &source);
    let prefix = "x".repeat(grapt_session::notice::MAX_NOTICE_TEXT_BYTES);
    let excluded = ["first", "second"]
        .into_iter()
        .map(|suffix| ExcludedTool {
            name: format!("{prefix}{suffix}"),
            reason: "malformed schema".into(),
        })
        .collect();
    let written = record_tool_excluded(&mut store, &authorities, &session, excluded)?
        .ok_or("initial notice absent")?;
    assert_eq!(
        written.excluded.len() + written.omitted,
        2,
        "display bounding silently lost an excluded definition"
    );
    let read = read_notices(store.connection(), &session)?.tool_excluded;
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].excluded.len() + read[0].omitted, 2);
    Ok(())
}

#[test]
fn full_names_determine_identity_before_the_display_bound() -> Result<(), Box<dyn std::error::Error>>
{
    let (mut store, source, session) = fixture()?;
    let authorities = DeterministicAuthorities::new(&source, &source);
    let prefix = "x".repeat(grapt_session::notice::MAX_NOTICE_TEXT_BYTES);
    for suffix in ["first", "second"] {
        record_tool_excluded(
            &mut store,
            &authorities,
            &session,
            vec![ExcludedTool {
                name: format!("{prefix}{suffix}"),
                reason: "malformed schema".into(),
            }],
        )?
        .ok_or("a changed full name was treated as unchanged")?;
    }
    assert_eq!(
        read_notices(store.connection(), &session)?
            .tool_excluded
            .len(),
        2
    );
    Ok(())
}

#[test]
fn repaired_exclusions_clear_once_and_remain_readable() -> Result<(), Box<dyn std::error::Error>> {
    let (mut store, source, session) = fixture()?;
    let authorities = DeterministicAuthorities::new(&source, &source);
    record_tool_excluded(&mut store, &authorities, &session, exclusions("z_last"))?;
    let cleared = record_tool_excluded(&mut store, &authorities, &session, vec![])?
        .ok_or("repair was not recorded")?;
    assert!(cleared.excluded.is_empty());
    assert_eq!(cleared.omitted, 0);
    assert!(record_tool_excluded(&mut store, &authorities, &session, vec![])?.is_none());
    assert_eq!(
        read_notices(store.connection(), &session)?
            .tool_excluded
            .len(),
        2
    );
    Ok(())
}

//! Witnesses for per-Session catalog snapshot custody.
//!
//! Executions run concurrently, so these interleave two Sessions in the exact
//! order a single latest-wins slot got wrong.

use super::*;

fn session(raw: &str) -> SessionId {
    match raw.parse() {
        Ok(session) => session,
        Err(_) => panic!("{raw:?} must be a usable session id"),
    }
}

fn catalog(name: &str) -> Arc<super::super::mcp::PublishedTools> {
    Arc::new(super::super::mcp::PublishedTools {
        definitions: vec![grapt_provider::ToolDefinition {
            input_format: grapt_provider::ToolInputFormat::Json,
            name: name.to_owned(),
            description: None,
            input_schema: "{}".to_owned(),
        }],
        ..Default::default()
    })
}

fn names(published: &super::super::mcp::PublishedTools) -> Vec<&str> {
    published
        .definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect()
}

/// Executions run CONCURRENTLY. This interleaves two Sessions in the exact
/// order a single latest-wins slot got wrong: A snapshots, B snapshots
/// over it, and only then does A read.
///
/// Under the old shared slot A would have planned against B's catalog —
/// a model offered a server the user never configured for that project,
/// and a call bound to the wrong authority.
#[test]
fn two_sessions_snapshotting_out_of_order_each_read_their_own_catalog() {
    let snapshots = PublishedSnapshots::default();
    let (a, b) = (
        session("ses_aaaaaaaaaaaaaaaaaaaaaaaaaa"),
        session("ses_bbbbbbbbbbbbbbbbbbbbbbbbbb"),
    );

    snapshots.set(&a, catalog("alpha_tool"));
    snapshots.set(&b, catalog("beta_tool"));

    assert_eq!(names(&snapshots.get(&a)), vec!["alpha_tool"]);
    assert_eq!(names(&snapshots.get(&b)), vec!["beta_tool"]);
}

/// One Session's FAILED snapshot must not empty another's plan. A failed
/// read stores an empty catalog, and under a shared slot that empty would
/// have been what the other Session planned against.
#[test]
fn a_failed_snapshot_empties_only_its_own_session() {
    let snapshots = PublishedSnapshots::default();
    let (a, b) = (
        session("ses_aaaaaaaaaaaaaaaaaaaaaaaaaa"),
        session("ses_bbbbbbbbbbbbbbbbbbbbbbbbbb"),
    );

    snapshots.set(&a, catalog("alpha_tool"));
    // B's configuration could not be read: empty, which is the honest
    // answer for B and must say nothing about A.
    snapshots.set(&b, Arc::new(super::super::mcp::PublishedTools::default()));

    assert_eq!(names(&snapshots.get(&a)), vec!["alpha_tool"]);
    assert!(snapshots.get(&b).definitions.is_empty());
}

/// A Session that never snapshotted publishes NOTHING rather than
/// inheriting whatever another Session left behind.
#[test]
fn a_session_with_no_snapshot_borrows_no_other_sessions_tools() {
    let snapshots = PublishedSnapshots::default();
    snapshots.set(
        &session("ses_aaaaaaaaaaaaaaaaaaaaaaaaaa"),
        catalog("alpha_tool"),
    );
    assert!(snapshots
        .get(&session("ses_cccccccccccccccccccccccccc"))
        .definitions
        .is_empty());
}

/// The guard removes the snapshot however the execution ends, so a later
/// run cannot inherit a catalog it never took.
#[test]
fn a_snapshot_does_not_outlive_its_execution() {
    let snapshots = PublishedSnapshots::default();
    let a = session("ses_aaaaaaaaaaaaaaaaaaaaaaaaaa");
    snapshots.set(&a, catalog("alpha_tool"));
    {
        let _guard = PublishedSnapshotGuard {
            snapshots: snapshots.clone(),
            session: a.clone(),
        };
        assert_eq!(names(&snapshots.get(&a)), vec!["alpha_tool"]);
    }
    assert!(
        snapshots.get(&a).definitions.is_empty(),
        "the snapshot is gone once the execution's guard drops"
    );
}

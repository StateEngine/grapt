use super::*;
use grapt_types::ProjectId;
use std::sync::atomic::{AtomicUsize, Ordering};

fn session(id: &str) -> Result<SessionId, String> {
    id.parse().map_err(|_| format!("{id} is no session id"))
}

fn resolved(directory: &str) -> Result<ResolvedLocation, String> {
    Ok(ResolvedLocation {
        directory: directory.to_owned(),
        project: ProjectId::new("global").ok_or("project")?,
        project_directory: directory.to_owned(),
    })
}

/// A discovery that times out on its first `failures` calls, counting them.
fn flaky(
    calls: &AtomicUsize,
    failures: usize,
) -> impl Fn(&str) -> std::future::Ready<Result<ResolvedLocation, String>> + '_ {
    move |directory| {
        let call = calls.fetch_add(1, Ordering::SeqCst);
        std::future::ready(if call < failures {
            Err("location discovery deadline exceeded".to_owned())
        } else {
            resolved(directory)
        })
    }
}

#[tokio::test]
async fn a_timed_out_discovery_is_retried_and_the_turn_resolves() -> Result<(), String> {
    let locations = SessionLocations::default();
    let calls = AtomicUsize::new(0);
    let location = locations
        .resolve_with(&session("ses_rel11_retry")?, "/work", flaky(&calls, 2))
        .await?;
    assert_eq!(location.directory, "/work");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    Ok(())
}

#[tokio::test]
async fn a_resolved_location_is_reused_without_discovery() -> Result<(), String> {
    let locations = SessionLocations::default();
    let session = session("ses_rel11_reuse")?;
    let calls = AtomicUsize::new(0);
    let first = locations
        .resolve_with(&session, "/work", flaky(&calls, 0))
        .await?;
    // Every later discovery would time out: the turn must not need one.
    let failing = AtomicUsize::new(0);
    let again = locations
        .resolve_with(&session, "/work", flaky(&failing, usize::MAX))
        .await?;
    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!(failing.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn a_changed_directory_is_discovered_again() -> Result<(), String> {
    let locations = SessionLocations::default();
    let session = session("ses_rel11_moved")?;
    let calls = AtomicUsize::new(0);
    locations
        .resolve_with(&session, "/work", flaky(&calls, 0))
        .await?;
    let moved = locations
        .resolve_with(&session, "/elsewhere", flaky(&calls, 0))
        .await?;
    assert_eq!(moved.directory, "/elsewhere");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn sessions_do_not_share_a_location() -> Result<(), String> {
    let locations = SessionLocations::default();
    let calls = AtomicUsize::new(0);
    locations
        .resolve_with(&session("ses_rel11_one")?, "/work", flaky(&calls, 0))
        .await?;
    locations
        .resolve_with(&session("ses_rel11_two")?, "/work", flaky(&calls, 0))
        .await?;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn a_discovery_that_keeps_failing_refuses_after_three_attempts() -> Result<(), String> {
    let locations = SessionLocations::default();
    let session = session("ses_rel11_down")?;
    let calls = AtomicUsize::new(0);
    let error = locations
        .resolve_with(&session, "/work", flaky(&calls, usize::MAX))
        .await
        .err()
        .ok_or("a discovery that never succeeded resolved")?;
    assert_eq!(error, "location discovery deadline exceeded");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    // A failure is not remembered: the next turn discovers afresh.
    locations
        .resolve_with(&session, "/work", flaky(&calls, 0))
        .await?;
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    Ok(())
}

fn git_init(directory: &Path) -> Result<(), String> {
    let status = std::process::Command::new("git")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .args(["init", "--quiet"])
        .current_dir(directory)
        .status()
        .map_err(|error| error.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("git init failed in {}", directory.display()))
}

/// A repository created inside the Session's own
/// directory after a turn moves its project root there, as discovery on
/// every turn did, so the next turn reads the nested repository's AGENTS
/// files and not the outer one's.
#[tokio::test]
async fn a_nested_repository_made_in_the_same_directory_is_discovered() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("grapt-rel11-nested-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let inner = root.join("inner");
    std::fs::create_dir_all(&inner).map_err(|error| error.to_string())?;
    let outcome = async {
        git_init(&root)?;
        let directory = inner.to_str().ok_or("fixture is not UTF-8")?;
        let locations = SessionLocations::default();
        let session = session("ses_rel11_nested")?;
        let canonical = |path: &str| std::fs::canonicalize(path).map_err(|error| error.to_string());
        let outer = locations.resolve(&session, directory).await?;
        assert_eq!(
            canonical(&outer.project_directory)?,
            canonical(root.to_str().ok_or("fixture is not UTF-8")?)?
        );
        let again = locations.resolve(&session, directory).await?;
        assert!(
            Arc::ptr_eq(&outer, &again),
            "an unchanged repository was discovered again"
        );
        git_init(&inner)?;
        let nested = locations.resolve(&session, directory).await?;
        assert_eq!(canonical(&nested.project_directory)?, canonical(directory)?);
        Ok::<_, String>(())
    }
    .await;
    let _ = std::fs::remove_dir_all(&root);
    outcome
}

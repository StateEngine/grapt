//! Real exec readiness under bounded concurrent spawn pressure; capture only.
use super::process_fixture::{observe_spawned_child, ChildObservation, CHILD_LIFETIME_SECS};
use super::*;

fn spawned_child_sample() -> ChildObservation {
    let mut child = Command::new("sleep")
        .arg(CHILD_LIFETIME_SECS.to_string())
        .spawn()
        .unwrap_or_else(|error| panic!("sleep could not start: {error}"));
    let pid = child.id();
    let observed = observe_spawned_child(&child);
    child
        .kill()
        .unwrap_or_else(|error| panic!("sleep kill: {error}"));
    child
        .wait()
        .unwrap_or_else(|error| panic!("sleep reap: {error}"));
    let observed = observed.unwrap_or_else(|error| panic!("sleep readiness: {error}"));
    assert_eq!(observed.unavailable, None, "{observed:?}");
    observed
        .children
        .iter()
        .find(|found| found.pid == pid)
        .cloned()
        .unwrap_or_else(|| panic!("spawned child missing: {observed:?}"))
}

/// Run explicitly:
/// `flock -o /home/example/secode-heavy.lock scripts/shared-cargo.sh test -p grapt-product --all-features --test capture cmdline_stress::spawned_child_cmdline_readiness_survives_stress -- --ignored --exact --show-output`
#[test]
#[ignore = "live exec stress; run explicitly as documented"]
fn spawned_child_cmdline_readiness_survives_stress() {
    const WORKERS: usize = 8;
    const SAMPLES: usize = 128;
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            thread::spawn(|| {
                let mut empty = 0;
                for _ in 0..SAMPLES {
                    let found = spawned_child_sample();
                    assert_eq!(found.comm.as_deref(), Ok("sleep"), "{found:?}");
                    assert!(!found.state.is_empty() && found.state != "?", "{found:?}");
                    if found.cmdline.is_empty() {
                        empty += 1;
                    } else {
                        assert!(found.cmdline.starts_with("sleep 5"), "{found:?}");
                    }
                }
                empty
            })
        })
        .collect();
    let empty: usize = workers
        .into_iter()
        .map(|worker| {
            worker
                .join()
                .unwrap_or_else(|error| panic!("stress worker failed: {error:?}"))
        })
        .sum();
    eprintln!(
        "live cmdline stress: {empty}/{} empty final samples",
        WORKERS * SAMPLES
    );
    assert_eq!(
        empty,
        0,
        "LIVE_EMPTY_CMDLINE_FINAL_SAMPLE: {empty}/{}",
        WORKERS * SAMPLES
    );
}

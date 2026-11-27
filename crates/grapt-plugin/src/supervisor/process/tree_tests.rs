use super::*;
use crate::protocol::{DEFAULT_BRIDGE_LIMITS, DEFAULT_SUPERVISOR_BUDGET};
use std::time::Duration;

fn entry(pid: u32, parent: u32, group: u32) -> Entry {
    Entry {
        pid,
        parent: Some(parent),
        group: Some(group),
        started: u64::from(pid) * 10,
        rss: 1024,
        cpu_millis: 0,
        readable: true,
    }
}

fn table(entries: &[Entry]) -> Table {
    entries.iter().map(|entry| (entry.pid, *entry)).collect()
}

fn pids(entries: &[Entry]) -> Vec<u32> {
    entries.iter().map(|entry| entry.pid).collect()
}

/// Leader 100 (its own group), a forked child, a `setsid` grandchild under
/// the chain, a group member whose parent died, and unrelated processes.
fn bridge_table() -> Table {
    table(&[
        entry(1, 0, 1),
        entry(50, 1, 50),
        entry(100, 50, 100),
        entry(101, 100, 100),
        entry(102, 101, 102),
        entry(103, 1, 100),
        entry(200, 1, 200),
    ])
}

#[test]
fn the_tree_is_the_group_plus_every_descendant_by_parent() {
    assert_eq!(
        pids(&members(&bridge_table(), 100)),
        vec![100, 101, 102, 103]
    );
    let cyclic = table(&[entry(7, 8, 7), entry(8, 7, 8)]);
    assert_eq!(pids(&members(&cyclic, 100)), Vec::<u32>::new());
}

#[test]
fn an_escape_is_a_tracked_process_alive_outside_the_tree_never_a_reused_pid() {
    let mut table = bridge_table();
    let tracked: Tracked = members(&table, 100)
        .iter()
        .map(|entry| (entry.pid, identity(&table, entry)))
        .collect();
    let recorded = tracked.get(&102).copied();
    // 102 re-parents to launchd after 101 dies; 103 dies; 101's PID is reused.
    table.remove(&103);
    table.insert(102, entry(102, 1, 102));
    table.insert(
        101,
        Entry {
            started: 999,
            ..entry(101, 1, 101)
        },
    );
    let now = members(&table, 100);
    assert_eq!(pids(&now), vec![100]);
    let escapees = escaped(&tracked, &table, &now);
    assert_eq!(escapees.keys().copied().collect::<Vec<_>>(), vec![102]);
    // Recorded under 101, which started at 1010.
    assert_eq!(
        escapees.get(&102).copied(),
        recorded.filter(
            |identity| identity.parent == Some(101) && identity.parent_started == Some(1010)
        )
    );
}

fn limits() -> BridgeLimits {
    BridgeLimits {
        rss_bytes: 10_000,
        tasks: 4,
        cpu_percent: 50,
        cpu_violation_samples: 2,
        ..DEFAULT_BRIDGE_LIMITS
    }
}

fn verdict(judgment: Judgment) -> Verdict {
    judgment
        .verdict
        .unwrap_or_else(|| panic!("the sample decided nothing"))
}

fn assert_named(reason: &str, phrases: &[&str]) {
    for phrase in phrases.iter().chain([&WATCHDOG_CONTAINMENT]) {
        assert!(reason.contains(phrase), "{reason:?} lacks {phrase:?}");
    }
}

#[test]
fn resident_memory_past_the_threshold_is_a_named_watchdog_resource_verdict() {
    let mut table = bridge_table();
    let mut watch = Watch::new(100, &limits(), 2.0);
    assert_eq!(watch.judge(&table, Instant::now()).verdict, None);
    if let Some(child) = table.get_mut(&102) {
        child.rss = 9_000;
    }
    let Verdict::Resource(reason) = verdict(watch.judge(&table, Instant::now())) else {
        panic!("memory past the threshold was not a resource verdict")
    };
    assert_named(
        &reason,
        &[
            "RSS 12072",
            "exceeded the observed",
            "termination threshold",
        ],
    );
}

#[test]
fn a_tree_past_its_process_bound_is_a_resource_verdict() {
    let mut table = bridge_table();
    table.insert(104, entry(104, 100, 100));
    let mut watch = Watch::new(100, &limits(), 2.0);
    let Verdict::Resource(reason) = verdict(watch.judge(&table, Instant::now())) else {
        panic!("a fifth process was not a resource verdict")
    };
    assert_named(&reason, &["5 processes", "4-process bound"]);
}

#[test]
fn a_process_that_leaves_the_tree_after_it_was_seen_is_reported_and_named_for_killing() {
    let mut table = bridge_table();
    let mut watch = Watch::new(100, &limits(), 2.0);
    assert_eq!(watch.judge(&table, Instant::now()).verdict, None);
    table.remove(&101);
    table.insert(102, entry(102, 1, 102));
    let judgment = watch.judge(&table, Instant::now());
    // The escapee stays tracked, so the teardown still kills it.
    assert_eq!(
        watch.tracked.get(&102).map(|identity| identity.started),
        Some(1020)
    );
    let Verdict::Other(reason) = verdict(judgment) else {
        panic!("an escape was not reported")
    };
    assert_named(
        &reason,
        &["left the bridge's process group and parent chain"],
    );
}

#[test]
fn a_leader_missing_from_three_samples_fails_closed() {
    let empty = Table::new();
    let mut watch = Watch::new(100, &limits(), 2.0);
    assert_eq!(watch.judge(&empty, Instant::now()).verdict, None);
    assert_eq!(watch.judge(&empty, Instant::now()).verdict, None);
    let Verdict::Other(reason) = verdict(watch.judge(&empty, Instant::now())) else {
        panic!("lost telemetry did not fail closed")
    };
    assert_named(&reason, &["lost the bridge leader", "3 samples"]);
}

#[test]
fn sustained_cpu_over_the_quota_across_the_window_is_a_resource_verdict() {
    let start = Instant::now();
    let mut table = bridge_table();
    let mut watch = Watch::new(100, &limits(), 2.0);
    // 0.5 cores allowed; 450 CPU-ms per 500 ms of wall time is 0.9 cores.
    for (step, cpu) in [(0, 0), (1, 450)] {
        if let Some(leader) = table.get_mut(&100) {
            leader.cpu_millis = cpu;
        }
        let at = start + Duration::from_millis(500 * step);
        assert_eq!(watch.judge(&table, at).verdict, None, "step {step}");
    }
    if let Some(leader) = table.get_mut(&100) {
        leader.cpu_millis = 900;
    }
    let Verdict::Resource(reason) =
        verdict(watch.judge(&table, start + Duration::from_millis(1000)))
    else {
        panic!("sustained CPU was not a resource verdict")
    };
    assert_named(&reason, &["CPU", "50% CPU quota", "2 samples"]);
    let mut idle = Watch::new(100, &limits(), 2.0);
    for step in 0..4 {
        let at = start + Duration::from_millis(500 * step);
        assert_eq!(idle.judge(&bridge_table(), at).verdict, None);
    }
}

#[test]
fn the_aggregate_terminates_the_largest_tree_past_the_supervisor_bounds() {
    let aggregate = Aggregate::new(&SupervisorBudget {
        rss_bytes: 1_000,
        tasks: 6,
        ..DEFAULT_SUPERVISOR_BUDGET
    });
    assert_eq!(aggregate.judge(&[(1, 400, 3), (2, 600, 3)]), None);
    let Some((leader, Verdict::Resource(reason))) = aggregate.judge(&[(1, 400, 3), (2, 601, 3)])
    else {
        panic!("aggregate memory past the budget was not judged")
    };
    assert_eq!(leader, 2);
    assert_named(
        &reason,
        &["1001 bytes RSS", "1000-byte aggregate threshold"],
    );
    let Some((leader, Verdict::Resource(reason))) = aggregate.judge(&[(1, 10, 5), (2, 10, 2)])
    else {
        panic!("aggregate processes past the budget were not judged")
    };
    assert_eq!(leader, 1);
    assert_named(&reason, &["7 processes", "6-process aggregate bound"]);
}

#[test]
fn a_sample_reports_the_tree_it_judged_and_the_public_kind_names_the_same_limits() {
    let mut watch = Watch::new(100, &limits(), 2.0);
    let judgment = watch.judge(&bridge_table(), Instant::now());
    assert_eq!((judgment.rss, judgment.tasks), (4 * 1024, 4));
    assert_eq!(
        crate::ContainmentKind::Watchdog.description(),
        WATCHDOG_CONTAINMENT
    );
}

#[test]
fn exited_workers_keep_their_cpu_charge_so_churn_cannot_hide_sustained_use() {
    // The review probe: a fresh worker burns 500 CPU-ms in each 500 ms sample
    // and exits, so the live members' lifetime CPU never grows.
    let start = Instant::now();
    let mut watch = Watch::new(100, &limits(), 2.0);
    let mut verdict = None;
    for step in 0..10u32 {
        let mut table = table(&[entry(1, 0, 1), entry(100, 1, 100)]);
        table.insert(
            1000 + step,
            Entry {
                cpu_millis: 500,
                ..entry(1000 + step, 100, 100)
            },
        );
        let at = start + Duration::from_millis(500 * u64::from(step));
        verdict = watch.judge(&table, at).verdict;
        if verdict.is_some() {
            break;
        }
    }
    let Some(Verdict::Resource(reason)) = verdict else {
        panic!("rotating workers hid sustained CPU use: {verdict:?}")
    };
    assert_named(&reason, &["CPU", "50% CPU quota"]);
}

#[test]
fn a_live_process_whose_telemetry_cannot_be_read_fails_closed_by_name() {
    let mut table = bridge_table();
    if let Some(child) = table.get_mut(&101) {
        child.readable = false;
    }
    let mut watch = Watch::new(100, &limits(), 2.0);
    assert_eq!(watch.judge(&table, Instant::now()).verdict, None);
    assert_eq!(watch.judge(&table, Instant::now()).verdict, None);
    // A readable sample resets the count; three unreadable in a row do not.
    assert_eq!(watch.judge(&bridge_table(), Instant::now()).verdict, None);
    assert_eq!(watch.judge(&table, Instant::now()).verdict, None);
    assert_eq!(watch.judge(&table, Instant::now()).verdict, None);
    let Verdict::Other(reason) = verdict(watch.judge(&table, Instant::now())) else {
        panic!("unreadable telemetry did not fail closed")
    };
    assert_named(
        &reason,
        &["unreadable for 1 live bridge process(es)", "3 samples"],
    );
}

#[test]
fn the_pinned_sentinel_is_neither_charged_nor_counted() {
    let mut table = bridge_table();
    table.insert(
        150,
        Entry {
            rss: 9_000,
            ..entry(150, 1, 100)
        },
    );
    let mut watch = Watch::new(100, &limits(), 2.0).pinned_by(150);
    let judgment = watch.judge(&table, Instant::now());
    assert_eq!(judgment.verdict, None);
    assert_eq!((judgment.rss, judgment.tasks), (4 * 1024, 4));
    assert!(!watch.tracked.contains_key(&150));
}

/// SC-MAC-02b: a journalled launch's sentinel (150) leads the group; the
/// bridge leader (100) is a member. The tree is the group plus the leader's
/// descendants, and the sentinel is neither charged nor counted.
#[test]
fn a_sentinel_led_groups_tree_is_the_group_and_the_leaders_descendants() {
    let table = table(&[
        entry(1, 0, 1),
        entry(150, 1, 150),
        entry(100, 1, 150),
        entry(101, 100, 150),
        entry(102, 101, 102),
        entry(103, 1, 150),
        entry(200, 1, 200),
    ]);
    assert_eq!(
        pids(&members_of(&table, 150, 100)),
        vec![100, 101, 102, 103, 150]
    );
    let mut watch = Watch::new(100, &limits(), 2.0).grouped(150).pinned_by(150);
    let judgment = watch.judge(&table, Instant::now());
    assert_eq!(judgment.verdict, None);
    assert_eq!(judgment.tasks, 4);
    assert!(!watch.tracked.contains_key(&150));
}

//! Native macOS witnesses for the watchdog containment (SC-MAC-02). The
//! decisions and every signal refusal are proven on all platforms with fakes
//! in `tree` and `signal`; these prove the process table, the group custody
//! and the kills on a real Darwin kernel. Every kill here goes through the
//! same gate as production.

use super::*;
use std::process::Child;
use std::sync::atomic::AtomicU64;

const FIXTURE_OUTPUT_BOUND: Duration = Duration::from_secs(10);
/// The fixtures' declared kill-and-reap bound, as `BridgeLimits::reap_millis`.
const FIXTURE_REAP: Duration = Duration::from_secs(5);
const FIXTURE_OUTPUT_BYTES: u64 = 4096;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

/// A `/bin/sh` script leading its own process group, pinned by a sentinel of
/// its own. It writes `label pid` lines to a file the test polls within a
/// bound, so no pipe or reader thread outlives it. Only declared labels are
/// read, each must be a signallable bridge ID that is alive under the
/// fixture, and dropping the fixture kills its group on the sentinel's
/// authority and its recorded processes by identity, through the gate.
struct Fixture {
    child: Option<Child>,
    leader: u32,
    floor: u64,
    custody: Custody,
    owned: Tracked,
    output: std::path::PathBuf,
}

impl Fixture {
    fn start(script: &str, labels: &[&str]) -> Result<Self, String> {
        let mut fixture = Self::spawn(script)?;
        let pids = fixture.read_pids_within(labels, FIXTURE_OUTPUT_BOUND)?;
        let table = snapshot(&mut System::new());
        for (label, pid) in pids {
            let entry = table
                .get(&pid)
                .ok_or(format!("fixture {label} {pid} is not alive"))?;
            fixture.owned.insert(pid, tree::identity(&table, entry));
        }
        Ok(fixture)
    }

    /// Start the script and pin its group; nothing is read yet.
    fn spawn(script: &str) -> Result<Self, String> {
        let output = std::env::temp_dir().join(format!(
            "secode-plugin-fixture-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let child = Command::new("/bin/sh")
            .args(["-c", script])
            .env("FIXTURE_OUT", &output)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        let leader = child.id();
        let mut fixture = Self {
            child: Some(child),
            leader,
            floor: 0,
            custody: Arc::new(Mutex::new(None)),
            owned: Tracked::new(),
            output,
        };
        let sentinel = pin_group(leader).map_err(|error| error.to_string())?;
        if let Ok(mut held) = fixture.custody.lock() {
            *held = Some(sentinel);
        }
        fixture.floor = match signal::Probe::observe(&Live, leader) {
            Seen::Alive { started, .. } => started,
            other => return Err(format!("fixture leader unreadable: {other:?}")),
        };
        Ok(fixture)
    }

    /// Poll the output file for every declared label within `bound`.
    fn read_pids_within(
        &self,
        labels: &[&str],
        bound: Duration,
    ) -> Result<BTreeMap<String, u32>, String> {
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            let mut pids = BTreeMap::new();
            if let Ok(file) = std::fs::File::open(&self.output) {
                let mut text = String::new();
                std::io::Read::read_to_string(
                    &mut std::io::Read::take(file, FIXTURE_OUTPUT_BYTES),
                    &mut text,
                )
                .map_err(|error| error.to_string())?;
                for line in text.lines() {
                    let Some((label, raw)) = line.split_once(' ') else {
                        continue;
                    };
                    if !labels.contains(&label) {
                        continue;
                    }
                    let raw = raw
                        .trim()
                        .parse::<u32>()
                        .map_err(|error| error.to_string())?;
                    signal::bridge_id(raw, own())
                        .map_err(|refusal| format!("fixture {label}: {refusal}"))?;
                    pids.insert(label.to_owned(), raw);
                }
            }
            if pids.len() == labels.len() {
                return Ok(pids);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("fixture did not report its pids within its bound".to_owned());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn pid(&self, label_pid: u32) -> Result<u32, String> {
        self.owned
            .contains_key(&label_pid)
            .then_some(label_pid)
            .ok_or_else(|| format!("{label_pid} is not a fixture process"))
    }

    /// Hand the leader to production custody; the group stays pinned here.
    fn take_child(&mut self) -> Result<Child, String> {
        self.child
            .take()
            .ok_or_else(|| "fixture leader already taken".to_owned())
    }

    /// Kill and reap the leader within the declared bound.
    fn reap(&mut self) -> Result<(), String> {
        release(&mut self.child, FIXTURE_REAP, "fixture leader").map_err(|error| error.to_string())
    }
}

impl Drop for Fixture {
    /// Every step is bounded: a failed test never hangs in cleanup.
    fn drop(&mut self) {
        let _ = kill_tree(
            self.leader,
            self.leader,
            self.floor,
            &self.owned,
            &self.custody,
            FIXTURE_REAP,
            None,
        );
        let _ = release(&mut self.child, FIXTURE_REAP, "fixture leader");
        let _ = std::fs::remove_file(&self.output);
    }
}

/// Every label's pid from the fixture's recorded processes.
fn labelled(fixture: &Fixture, labels: &[&str]) -> Result<Vec<u32>, String> {
    let pids = fixture.read_pids_within(labels, FIXTURE_OUTPUT_BOUND)?;
    labels
        .iter()
        .map(|label| {
            pids.get(*label)
                .copied()
                .ok_or(format!("no {label} pid"))
                .and_then(|pid| fixture.pid(pid))
        })
        .collect()
}

fn alive(pid: u32) -> bool {
    !matches!(signal::Probe::observe(&Live, pid), Seen::Gone)
}

/// Pin a fixture group led by its own leader with a `/bin/cat` sentinel of
/// the test's: the custody `kill_tree` signals a group on. (Production
/// sentinels lead their bridge's group instead; see `Spawner`.)
fn pin_group(leader: u32) -> Result<Child, PluginError> {
    let group = signal::bridge_id(leader, own())
        .map_err(|refusal| PluginError::Spawn(format!("refusing to pin group: {refusal}")))?;
    Command::new("/bin/cat")
        .env_clear()
        .process_group(group)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| PluginError::Spawn(format!("plugin group custody refused: {error}")))
}

/// A killed process outside the test's own children is reaped by launchd.
fn gone_within(pid: u32, bound: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + bound;
    while tokio::time::Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    !alive(pid)
}

/// Production custody of a fixture leader: its own sentinel and floor.
fn control_for(fixture: &mut Fixture) -> Result<ProcessControl, String> {
    let child = fixture.take_child()?;
    let leader = child.id();
    let sentinel = pin_group(leader).map_err(|error| error.to_string())?;
    Ok(ProcessControl {
        child: Arc::new(Mutex::new(Some(child))),
        violation: Arc::new(Mutex::new(None)),
        exit_observed: Arc::new(AtomicBool::new(false)),
        containment: ProcessContainment::Watchdog {
            group: leader,
            leader,
            floor: fixture.floor,
            tracked: Arc::new(Mutex::new(Tracked::new())),
            custody: Arc::new(Mutex::new(Some(sentinel))),
            record: None,
        },
        cleanup: Duration::from_millis(500),
        reap: Duration::from_millis(500),
    })
}

fn watched(control: ProcessControl) -> Watched {
    let ProcessContainment::Watchdog {
        leader, custody, ..
    } = &control.containment;
    let sentinel = custody
        .lock()
        .ok()
        .and_then(|held| held.as_ref().map(Child::id))
        .unwrap_or(0);
    let watch =
        Watch::new(*leader, &crate::protocol::DEFAULT_BRIDGE_LIMITS, 4.0).pinned_by(sentinel);
    Watched {
        control,
        watch,
        sample: Duration::from_millis(100),
    }
}

fn act(actions: Vec<Action>) -> Result<(), String> {
    for action in actions {
        match action {
            Action::Stop(control) => control.stop_containment(),
            Action::Terminate(control, Verdict::Resource(reason)) => {
                control.terminate_for(ViolationKind::Resource, reason)
            }
            Action::Terminate(control, Verdict::Other(reason)) => {
                control.terminate_for(ViolationKind::Other, reason)
            }
        }
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[test]
fn a_fixture_refuses_pid_zero_and_one_before_recording_or_signalling_them() {
    for script in [
        "echo ready 0 > \"$FIXTURE_OUT\"; exec sleep 60",
        "echo ready 1 > \"$FIXTURE_OUT\"; exec sleep 60",
    ] {
        let refused = Fixture::start(script, &["ready"]).err();
        assert!(
            refused
                .as_deref()
                .is_some_and(|error| error.contains("not a bridge process ID")),
            "{refused:?}"
        );
    }
}

#[test]
fn a_forked_descendant_is_in_the_tree_and_dies_with_it() -> Result<(), String> {
    let mut fixture = Fixture::start(
        "sleep 60 & echo child $! >> \"$FIXTURE_OUT\"; wait",
        &["child"],
    )?;
    let [child] = labelled(&fixture, &["child"])?[..] else {
        return Err("one child pid".to_owned());
    };
    let tree = tree::members(&snapshot(&mut System::new()), fixture.leader);
    let pids: Vec<u32> = tree.iter().map(|entry| entry.pid).collect();
    assert!(
        pids.contains(&fixture.leader) && pids.contains(&child),
        "{pids:?}"
    );
    kill_tree(
        fixture.leader,
        fixture.leader,
        fixture.floor,
        &Tracked::new(),
        &fixture.custody,
        FIXTURE_REAP,
        None,
    )
    .map_err(|error| error.to_string())?;
    fixture.reap()?;
    assert!(
        gone_within(child, Duration::from_secs(5)),
        "the descendant outlived its tree"
    );
    Ok(())
}

/// The first review HOLD on 2b6f0a72: the leader exits and is reaped before
/// any sample tracked its tree. Held custody still kills the group member it
/// left behind, through the watchdog's own tick.
#[test]
fn a_leader_that_exits_before_any_sample_cannot_strand_its_group() -> Result<(), String> {
    let mut fixture = Fixture::start("sleep 60 & echo child $! >> \"$FIXTURE_OUT\"", &["child"])?;
    let [child] = labelled(&fixture, &["child"])?[..] else {
        return Err("one child pid".to_owned());
    };
    let mut registry = vec![watched(control_for(&mut fixture)?)];
    let aggregate = Aggregate::new(&crate::protocol::DEFAULT_SUPERVISOR_BUDGET);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !registry.is_empty() && tokio::time::Instant::now() < deadline {
        let actions = tick(&mut registry, &snapshot(&mut System::new()), &aggregate);
        act(actions)?;
        thread::sleep(Duration::from_millis(20));
    }
    assert!(registry.is_empty(), "the exited leader stayed registered");
    assert!(
        gone_within(child, Duration::from_secs(5)),
        "the group outlived its reaped leader"
    );
    Ok(())
}

#[test]
fn without_custody_no_group_is_signalled() -> Result<(), String> {
    let fixture = Fixture::start(
        "sleep 60 & echo child $! >> \"$FIXTURE_OUT\"; wait",
        &["child"],
    )?;
    let [child] = labelled(&fixture, &["child"])?[..] else {
        return Err("one child pid".to_owned());
    };
    // No sentinel and nothing tracked: the group ID proves nothing, so the
    // teardown must leave the group alone (the fixture's own custody cleans up).
    kill_tree(
        fixture.leader,
        fixture.leader,
        fixture.floor,
        &Tracked::new(),
        &Arc::new(Mutex::new(None)),
        FIXTURE_REAP,
        None,
    )
    .map_err(|error| error.to_string())?;
    thread::sleep(Duration::from_millis(200));
    assert!(alive(child), "a group was signalled without custody");
    Ok(())
}

#[test]
fn a_setsid_child_is_contained_by_its_chain_and_reported_and_killed_once_it_escapes(
) -> Result<(), String> {
    let mut fixture = Fixture::start(
        // The escapee names itself only once it has left the group.
        "(perl -MPOSIX -e 'POSIX::setsid(); open(my $out, \">>\", $ENV{FIXTURE_OUT}) or die; \
         print $out \"escapee $$\\n\"; close $out; sleep 60' & sleep 60) & \
         echo parent $! >> \"$FIXTURE_OUT\"; wait",
        &["escapee", "parent"],
    )?;
    let [escapee, parent] = labelled(&fixture, &["escapee", "parent"])?[..] else {
        return Err("escapee and parent pids".to_owned());
    };
    let mut watch = Watch::new(fixture.leader, &crate::protocol::DEFAULT_BRIDGE_LIMITS, 4.0);
    let table = snapshot(&mut System::new());
    assert_ne!(
        table.get(&escapee).and_then(|entry| entry.group),
        Some(fixture.leader)
    );
    assert_eq!(
        watch.judge(&table, tokio::time::Instant::now()).verdict,
        None
    );
    assert!(watch.tracked.contains_key(&escapee));
    // Its parent dies: launchd adopts it, outside the group and the chain.
    let parent_identity = fixture
        .owned
        .get(&parent)
        .copied()
        .ok_or("parent identity")?;
    let parent_target = Target::Process {
        identity: parent_identity,
        floor: fixture.floor,
    };
    signal::signal(parent_target, own(), &Live, &mut Kernel)
        .map_err(|refusal| refusal.to_string())?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let verdict = loop {
        let judged = watch.judge(&snapshot(&mut System::new()), tokio::time::Instant::now());
        if judged.verdict.is_some() || tokio::time::Instant::now() >= deadline {
            break judged.verdict;
        }
        thread::sleep(Duration::from_millis(50));
    };
    let Some(Verdict::Other(reason)) = verdict else {
        panic!("an escaped process was not reported: {verdict:?}")
    };
    assert!(
        reason.contains("left the bridge's process group and parent chain"),
        "{reason}"
    );
    assert!(reason.contains(tree::WATCHDOG_CONTAINMENT), "{reason}");
    kill_tree(
        fixture.leader,
        fixture.leader,
        fixture.floor,
        &watch.tracked,
        &fixture.custody,
        FIXTURE_REAP,
        None,
    )
    .map_err(|error| error.to_string())?;
    fixture.reap()?;
    assert!(
        gone_within(escapee, Duration::from_secs(5)),
        "the escapee survived the teardown"
    );
    Ok(())
}

#[test]
fn one_tick_terminates_the_largest_tree_past_the_aggregate_budget() -> Result<(), String> {
    let mut small = Fixture::start(
        "echo ready $$ >> \"$FIXTURE_OUT\"; exec sleep 60",
        &["ready"],
    )?;
    let mut large = Fixture::start(
        "sleep 60 & echo child $! >> \"$FIXTURE_OUT\"; sleep 60 & echo other $! >> \"$FIXTURE_OUT\"; wait",
        &["child", "other"],
    )?;
    let doomed = labelled(&large, &["child", "other"])?;
    let small_leader = small.leader;
    let mut registry = vec![
        watched(control_for(&mut small)?),
        watched(control_for(&mut large)?),
    ];
    // Three bridge processes against a two-process aggregate: the larger goes.
    let aggregate = Aggregate::new(&SupervisorBudget {
        tasks: 2,
        ..crate::protocol::DEFAULT_SUPERVISOR_BUDGET
    });
    let actions = tick(&mut registry, &snapshot(&mut System::new()), &aggregate);
    let [Action::Terminate(_, Verdict::Resource(reason))] = actions.as_slice() else {
        panic!("one aggregate verdict was expected")
    };
    assert!(reason.contains("aggregate bound"), "{reason}");
    act(actions)?;
    for pid in doomed {
        assert!(gone_within(pid, Duration::from_secs(5)), "{pid} survived");
    }
    assert_eq!(registry.len(), 1, "the terminated tree left the registry");
    assert_eq!(registry[0].watch.leader, small_leader);
    for entry in registry.drain(..) {
        entry
            .control
            .terminate()
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The review HOLD on 39f5f9f9: a fixture whose pids never arrive fails within
/// its read bound, and dropping it still kills its whole group and reaps its
/// leader within the reap bound.
#[test]
fn a_fixture_that_fails_to_start_is_cleaned_up_within_its_bounds() -> Result<(), String> {
    let fixture = Fixture::spawn("sleep 60 & echo child $! >> \"$FIXTURE_OUT\"; exec sleep 60")?;
    let leader = fixture.leader;
    let started = tokio::time::Instant::now();
    let missing = fixture.read_pids_within(&["never"], Duration::from_millis(300));
    assert!(missing.is_err(), "an undeclared pid was reported");
    assert!(tokio::time::Instant::now().duration_since(started) < Duration::from_secs(5));
    let child = *fixture
        .read_pids_within(&["child"], FIXTURE_OUTPUT_BOUND)?
        .get("child")
        .ok_or("child pid")?;
    let dropped = tokio::time::Instant::now();
    drop(fixture);
    assert!(
        tokio::time::Instant::now().duration_since(dropped)
            < FIXTURE_REAP * 2 + Duration::from_secs(1),
        "fixture cleanup outlived its bounds"
    );
    assert!(
        gone_within(child, Duration::from_secs(5)),
        "the group outlived its failed fixture"
    );
    assert!(
        gone_within(leader, Duration::from_secs(5)),
        "the leader outlived its failed fixture"
    );
    Ok(())
}

/// The snapshot's group is real evidence: a member whose parent died, so its
/// whole parent chain no longer reaches the leader, is in the tree only by
/// its group.
#[test]
fn a_group_member_outside_the_parent_chain_is_in_the_tree_by_its_group() -> Result<(), String> {
    let fixture = Fixture::start(
        "(sleep 60 & echo orphan $! >> \"$FIXTURE_OUT\"); exec sleep 60",
        &["orphan"],
    )?;
    let [orphan] = labelled(&fixture, &["orphan"])?[..] else {
        return Err("one orphan pid".to_owned());
    };
    // The subshell exits on its own; wait, within a bound, until no ancestor
    // of the orphan is the leader.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let table = loop {
        let table = snapshot(&mut System::new());
        if !chain_reaches(&table, orphan, fixture.leader) {
            break table;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("the orphan's parent chain still reaches the leader".to_owned());
        }
        thread::sleep(Duration::from_millis(50));
    };
    let entry = table.get(&orphan).ok_or("the orphan is not in the table")?;
    assert_eq!(
        entry.group,
        Some(fixture.leader),
        "the snapshot lost the group"
    );
    let members: Vec<u32> = tree::members(&table, fixture.leader)
        .iter()
        .map(|member| member.pid)
        .collect();
    assert!(members.contains(&orphan), "{members:?}");
    Ok(())
}

/// Whether any ancestor of `pid` in `table` is `leader`, bounded by the table.
fn chain_reaches(table: &Table, pid: u32, leader: u32) -> bool {
    let mut current = table.get(&pid).and_then(|entry| entry.parent);
    for _ in 0..table.len() {
        match current {
            Some(parent) if parent == leader => return true,
            Some(parent) => current = table.get(&parent).and_then(|entry| entry.parent),
            None => return false,
        }
    }
    false
}

/// SC-MAC-02b: the exact sentinel contract on Darwin. argv[0] carries the
/// nonce tag through the census, the sentinel leads its own group, and the
/// INT_MAX interval keeps it alive (Apple's sleep accepts it).
#[test]
fn a_sentinel_carries_its_tag_leads_its_group_and_stays_alive() -> Result<(), String> {
    let tag = format!("{}smoke{}", journal::SENTINEL_TAG, std::process::id());
    let mut sentinel = Some(spawn_sentinel(&tag).map_err(|error| error.to_string())?);
    let pid = sentinel.as_ref().map(Child::id).ok_or("sentinel")?;
    thread::sleep(Duration::from_secs(1));
    let tagged = journal::Census::tagged(&LiveCensus, &tag, getuid().as_raw());
    let alive = matches!(signal::Probe::observe(&Live, pid), Seen::Alive { .. });
    let leads = signal::Probe::group_of(&Live, pid) == Some(pid);
    release(&mut sentinel, FIXTURE_REAP, "smoke sentinel").map_err(|error| error.to_string())?;
    assert_eq!(tagged, Some(vec![pid]), "the census did not find the tag");
    assert!(alive && leads, "alive {alive}, leads its group {leads}");
    Ok(())
}

/// SC-MAC-02b: a journalled launch whose owner is gone is reclaimed through
/// the real gate on Darwin. The owner is recorded with a start time that no
/// longer matches (a dead owner); the sentinel and its group member are this
/// test's children, so reaper threads stand in for launchd.
#[test]
fn a_journalled_orphan_is_reclaimed_by_its_group() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("grapt-reclaim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).map_err(|error| error.to_string())?;
    let result = journey(&root);
    // A failed journey keeps its journal for inspection.
    if result.is_ok() {
        let _ = std::fs::remove_dir_all(&root);
    }
    result
}

/// A launcher over the production spawns. Children whose release fails are
/// kept in `held`, never dropped.
struct Real {
    held: Held<Child>,
}

impl journal::Launcher for Real {
    type Sentinel = Child;
    type Bridge = Child;

    fn sentinel(&mut self, tag: &str) -> Result<(Child, Recorded), journal::NotLaunched> {
        let child = spawn_sentinel(tag).map_err(|error| journal::NotLaunched {
            error,
            nothing_left: true,
        })?;
        let pid = child.id();
        match signal::Probe::observe(&Live, pid) {
            Seen::Alive { started, .. } => Ok((child, Recorded { pid, started })),
            _ => {
                let mut held = Some(child);
                let released = release(&mut held, FIXTURE_REAP, "journey sentinel");
                self.held.0.extend(held);
                Err(journal::NotLaunched {
                    error: PluginError::Spawn("sentinel unreadable".to_owned()),
                    nothing_left: released.is_ok(),
                })
            }
        }
    }

    fn bridge(&mut self, group: u32) -> Result<Child, PluginError> {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        spawn_in_group(command, group)
    }

    fn abandon(&mut self, sentinel: Child) -> Result<(), PluginError> {
        let mut held = Some(sentinel);
        let released = release(&mut held, FIXTURE_REAP, "journey sentinel");
        self.held.0.extend(held);
        released
    }
}

fn journey(root: &std::path::Path) -> Result<(), String> {
    let uid = getuid().as_raw();
    let journal = Journal::open(&root.join("plugin-recovery"), uid).map_err(|e| e.to_string())?;
    let nonce = format!("journey{}", std::process::id());
    let Seen::Alive {
        started: own_started,
        ..
    } = signal::Probe::observe(&Live, std::process::id())
    else {
        return Err("own identity".to_owned());
    };
    let intent = Record {
        version: journal::RECORD_VERSION,
        nonce: nonce.clone(),
        uid,
        // A start that no longer matches: this owner reads as gone.
        owner: Recorded {
            pid: std::process::id(),
            started: own_started.saturating_sub(1),
        },
        intent_millis: 0,
        sentinel: None,
    };
    let mut real = Real {
        held: Held(Vec::new()),
    };
    let (sentinel, bridge, _) = journal::launch(&journal, &mut real, &LiveCensus, intent)
        .map_err(|error| error.to_string())?;
    let (sentinel_pid, bridge_pid) = (sentinel.id(), bridge.id());
    let owned = Arc::new(Mutex::new(Held(vec![sentinel, bridge])));
    let in_group = signal::Probe::group_of(&Live, bridge_pid);
    // launchd's part, bounded: reap whatever the reclaim kills.
    let reaper = {
        let owned = Arc::clone(&owned);
        thread::spawn(move || {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while tokio::time::Instant::now() < deadline {
                let Ok(mut owned) = owned.lock() else {
                    return;
                };
                owned.reap_some();
                if owned.0.is_empty() {
                    return;
                }
                drop(owned);
                thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let report = journal::reclaim(&journal, &LiveCensus, &mut Kernel, own(), 16, FIXTURE_REAP);
    let _ = reaper.join();
    let left = owned
        .lock()
        .map(|owned| owned.0.len())
        .unwrap_or(usize::MAX);
    let report = report.map_err(|error| error.to_string())?;
    assert_eq!(
        in_group,
        Some(sentinel_pid),
        "the bridge did not join the sentinel's group"
    );
    assert_eq!(report.reclaimed, vec![nonce], "{report:?}");
    assert_eq!(left, 0, "the reclaimed group outlived its signal");
    assert!(journal.scan(16).map_err(|e| e.to_string())?.is_empty());
    Ok(())
}

/// The review HOLD on bcba9142, natively: a sentinel whose release fails
/// (here a zero reap bound) stays held, and its record is not proven ended,
/// so it stays until the sentinel is actually reaped.
#[test]
fn a_failed_release_keeps_both_the_sentinel_and_its_record() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("grapt-release-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).map_err(|error| error.to_string())?;
    let journal = Journal::open(&root.join("plugin-recovery"), getuid().as_raw())
        .map_err(|e| e.to_string())?;
    let nonce = format!("release{}", std::process::id());
    let tag = format!("{}{nonce}", journal::SENTINEL_TAG);
    let sentinel = spawn_sentinel(&tag).map_err(|error| error.to_string())?;
    let pid = sentinel.id();
    let mut held = Held(vec![sentinel]);
    let Seen::Alive { started, .. } = signal::Probe::observe(&Live, pid) else {
        return Err("sentinel identity".to_owned());
    };
    let record = Record {
        version: journal::RECORD_VERSION,
        nonce,
        uid: getuid().as_raw(),
        owner: Recorded { pid: 1, started: 0 },
        intent_millis: 0,
        sentinel: Some(Recorded { pid, started }),
    };
    journal
        .publish(&record)
        .map_err(|error| error.to_string())?;
    // Killed but not observed reaped within a zero bound: it stays held.
    assert!(held.finish(Duration::ZERO).is_err());
    assert_eq!(
        held.0.len(),
        1,
        "a sentinel that survived its release was dropped"
    );
    assert!(journal::prove_ended(&record, &LiveCensus).is_none());
    assert_eq!(journal.scan(4).map_err(|e| e.to_string())?.len(), 1);
    // Once it is really reaped, the census proves the launch ended.
    held.finish(FIXTURE_REAP)
        .map_err(|error| error.to_string())?;
    let proof = journal::prove_ended(&record, &LiveCensus).ok_or("not proven ended")?;
    journal
        .retire_record(proof)
        .map_err(|error| error.to_string())?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

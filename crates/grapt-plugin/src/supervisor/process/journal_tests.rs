//! The journal, its reclaim decisions and the launch protocol, witnessed with
//! a fake process world and recorded signals: nothing here signals a process.

use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

const OWN: Own = Own {
    pid: 4000,
    group: 3900,
};
const OWNER: u32 = 700;
const SENTINEL: u32 = 800;
const BRIDGE: u32 = 801;
const REAP: Duration = Duration::from_millis(100);

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

fn new_scratch() -> (Scratch, u32) {
    let root = std::env::temp_dir().join(format!(
        "grapt-journal-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap_or_else(|error| panic!("scratch: {error}"));
    let uid = std::fs::metadata(&root)
        .unwrap_or_else(|error| panic!("scratch: {error}"))
        .uid();
    (Scratch(root), uid)
}

fn open_journal(scratch: &Scratch, uid: u32) -> Journal {
    Journal::open(&scratch.0.join("plugin-recovery"), uid)
        .unwrap_or_else(|error| panic!("journal: {error}"))
}

fn record(uid: u32, sentinel: Option<Recorded>) -> Record {
    Record {
        version: RECORD_VERSION,
        nonce: "nonce0123456789".to_owned(),
        uid,
        owner: Recorded {
            pid: OWNER,
            started: 50,
        },
        intent_millis: 100_000,
        sentinel,
    }
}

fn recorded_sentinel() -> Option<Recorded> {
    Some(Recorded {
        pid: SENTINEL,
        started: 100,
    })
}

#[derive(Clone)]
struct Proc {
    seen: Seen,
    group: u32,
    tag: Option<String>,
}

/// A process table: a killed group's members read as gone afterwards.
#[derive(Clone, Default)]
struct World {
    procs: BTreeMap<u32, Proc>,
    killed: Rc<RefCell<Vec<i32>>>,
    /// Group kills that take no effect (a group that will not end).
    stubborn: bool,
    tags_unreadable: bool,
    /// The group census cannot be completed.
    members_unknown: bool,
    /// Exited but not reaped: absent from the table, still held by the kernel.
    unreaped: Vec<u32>,
}

impl World {
    fn with(mut self, pid: u32, started: u64, parent: u32, group: u32, tag: Option<&str>) -> Self {
        self.procs.insert(
            pid,
            Proc {
                seen: Seen::Alive {
                    started,
                    parent: Some(parent),
                },
                group,
                tag: tag.map(str::to_owned),
            },
        );
        self
    }

    fn live(&self, pid: u32) -> Option<&Proc> {
        let proc = self.procs.get(&pid)?;
        let killed = !self.stubborn
            && self
                .killed
                .borrow()
                .iter()
                .any(|group| *group == i32::try_from(proc.group).unwrap_or(-1));
        (!killed).then_some(proc)
    }

    /// The orphaned launch: owner gone, sentinel adopted by launchd, bridge in its group.
    fn orphaned(tag: &str) -> Self {
        World::default()
            .with(SENTINEL, 100, 1, SENTINEL, Some(tag))
            .with(BRIDGE, 101, 1, SENTINEL, None)
    }
}

impl Probe for World {
    fn observe(&self, pid: u32) -> Seen {
        self.live(pid).map_or(Seen::Gone, |proc| proc.seen)
    }

    fn group_of(&self, pid: u32) -> Option<u32> {
        self.live(pid).map(|proc| proc.group)
    }
}

impl Census for World {
    fn members(&self, group: u32) -> Option<Vec<u32>> {
        if self.members_unknown {
            return None;
        }
        Some(
            self.procs
                .keys()
                .filter(|pid| self.live(**pid).is_some_and(|proc| proc.group == group))
                .copied()
                .collect(),
        )
    }

    fn tagged(&self, tag: &str, _uid: u32) -> Option<Vec<u32>> {
        if self.tags_unreadable {
            return None;
        }
        Some(
            self.procs
                .iter()
                .filter(|(pid, proc)| {
                    self.live(**pid).is_some() && proc.tag.as_deref() == Some(tag)
                })
                .map(|(pid, _)| *pid)
                .collect(),
        )
    }

    fn unreaped(&self, pid: u32) -> bool {
        self.unreaped.contains(&pid)
    }
}

struct Kills(Rc<RefCell<Vec<i32>>>);

impl Signals for Kills {
    fn kill(&mut self, pid: i32) -> Result<(), String> {
        Err(format!("reclaim must never signal a single process: {pid}"))
    }

    fn kill_group(&mut self, group: i32) -> Result<(), String> {
        self.0.borrow_mut().push(group);
        Ok(())
    }
}

fn reclaim_in(journal: &Journal, world: &World) -> (Reclaimed, Vec<i32>) {
    let mut kills = Kills(Rc::clone(&world.killed));
    let report = reclaim(journal, world, &mut kills, OWN, 16, REAP)
        .unwrap_or_else(|error| panic!("reclaim: {error}"));
    let killed = world.killed.borrow().clone();
    (report, killed)
}

fn tag() -> String {
    record(0, None).tag()
}

fn on_disk(journal: &Journal) -> Vec<Scanned> {
    journal
        .scan(16)
        .unwrap_or_else(|error| panic!("scan: {error}"))
}

#[test]
fn a_published_record_is_a_private_file_in_a_private_directory() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    let published = record(uid, recorded_sentinel());
    journal
        .publish(&published)
        .unwrap_or_else(|error| panic!("publish: {error}"));
    assert_eq!(on_disk(&journal), vec![Scanned::Record(published.clone())]);
    let dir = std::fs::metadata(scratch.0.join("plugin-recovery")).map(|meta| meta.mode() & 0o777);
    let file = std::fs::metadata(scratch.0.join("plugin-recovery/nonce0123456789.json"))
        .map(|meta| meta.mode() & 0o777);
    assert_eq!((dir.ok(), file.ok()), (Some(0o700), Some(0o600)));
}

#[test]
fn a_shared_journal_directory_is_refused() {
    let (scratch, uid) = new_scratch();
    let shared = scratch.0.join("plugin-recovery");
    std::fs::create_dir(&shared).unwrap_or_else(|error| panic!("dir: {error}"));
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|error| panic!("chmod: {error}"));
    assert!(Journal::open(&shared, uid).is_err());
    assert!(Journal::open(&shared, uid.wrapping_add(1)).is_err());
}

#[test]
fn foreign_corrupt_oversized_and_misnamed_records_are_preserved_unverified() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    let dir = scratch.0.join("plugin-recovery");
    let write = |name: &str, body: &[u8], mode: u32| {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap_or_else(|error| panic!("write: {error}"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            .unwrap_or_else(|error| panic!("chmod: {error}"));
    };
    let body = serde_json::to_vec(&record(uid, None)).unwrap_or_default();
    // Well formed and correctly named: only its group-readable mode refuses it.
    let foreign = serde_json::to_vec(&Record {
        nonce: "aaaaaaaaaa".to_owned(),
        ..record(uid, None)
    })
    .unwrap_or_default();
    write("aaaaaaaaaa.json", &foreign, 0o644);
    write("bbbbbbbbbb.json", b"{not json", 0o600);
    write("cccccccccc.json", &vec![b' '; 5000], 0o600);
    write("dddddddddd.json", &body, 0o600);
    std::os::unix::fs::symlink(dir.join("dddddddddd.json"), dir.join("eeeeeeeeee.json"))
        .unwrap_or_else(|error| panic!("symlink: {error}"));
    let (report, killed) = reclaim_in(&journal, &World::default());
    assert_eq!(report.unverified.len(), 5, "{report:?}");
    assert!(killed.is_empty());
    assert_eq!(
        on_disk(&journal).len(),
        5,
        "an unverified record was removed"
    );
}

#[test]
fn a_live_owners_record_is_skipped_and_an_unreadable_owner_preserves_it() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    journal
        .publish(&record(uid, recorded_sentinel()))
        .unwrap_or_else(|error| panic!("publish: {error}"));
    let live = World::orphaned(&tag()).with(OWNER, 50, 1, OWNER, None);
    let (report, killed) = reclaim_in(&journal, &live);
    assert_eq!((report.live.len(), killed.len()), (1, 0));
    let mut unreadable = World::orphaned(&tag());
    unreadable.procs.insert(
        OWNER,
        Proc {
            seen: Seen::Unreadable,
            group: OWNER,
            tag: None,
        },
    );
    let (report, killed) = reclaim_in(&journal, &unreadable);
    assert_eq!((report.unverified.len(), killed.len()), (1, 0));
    assert_eq!(on_disk(&journal).len(), 1);
}

#[test]
fn a_verified_orphan_is_reclaimed_with_one_group_signal_and_its_record_removed() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    journal
        .publish(&record(uid, recorded_sentinel()))
        .unwrap_or_else(|error| panic!("publish: {error}"));
    // The owner's PID now runs someone else: the owner is gone.
    let world = World::orphaned(&tag()).with(OWNER, 51, 1, OWNER, None);
    let (report, killed) = reclaim_in(&journal, &world);
    assert_eq!(report.reclaimed, vec!["nonce0123456789".to_owned()]);
    assert_eq!(killed, vec![i32::try_from(SENTINEL).unwrap_or(0)]);
    assert!(on_disk(&journal).is_empty());
}

#[test]
fn a_reused_sentinel_a_non_leader_sentinel_or_a_foreign_parent_is_preserved_unsignalled() {
    // The decision itself refuses each, before the gate's own re-check.
    for world in [
        World::default().with(SENTINEL, 999, 1, SENTINEL, Some(&tag())),
        World::default().with(SENTINEL, 100, 1, 900, Some(&tag())),
        World::default().with(SENTINEL, 100, 55, SENTINEL, Some(&tag())),
    ] {
        let decided = decide(&record(0, recorded_sentinel()), &world, OWN);
        assert!(matches!(decided, Decision::Unverified(_)), "{decided:?}");
    }
    let cases = [
        World::default().with(SENTINEL, 999, 1, SENTINEL, Some(&tag())),
        World::default().with(SENTINEL, 100, 1, 900, Some(&tag())),
        World::default().with(SENTINEL, 100, 55, SENTINEL, Some(&tag())),
    ];
    for world in cases {
        let (scratch, uid) = new_scratch();
        let journal = open_journal(&scratch, uid);
        journal
            .publish(&record(uid, recorded_sentinel()))
            .unwrap_or_else(|error| panic!("publish: {error}"));
        let (report, killed) = reclaim_in(&journal, &world);
        assert_eq!(
            (report.unverified.len(), killed.len()),
            (1, 0),
            "{report:?}"
        );
        assert_eq!(on_disk(&journal).len(), 1);
    }
}

#[test]
fn an_intent_only_record_names_its_sentinel_only_by_one_fitting_nonce() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    let intent = record(uid, None);
    // Exactly one fitting process carries the nonce: reclaimed.
    journal
        .publish(&intent)
        .unwrap_or_else(|error| panic!("publish: {error}"));
    let (report, killed) = reclaim_in(&journal, &World::orphaned(&tag()));
    assert_eq!((report.reclaimed.len(), killed.len()), (1, 1), "{report:?}");
    // A different nonce: nothing of this launch exists; stale, and retired
    // on the census proof.
    journal
        .publish(&intent)
        .unwrap_or_else(|error| panic!("publish: {error}"));
    let (report, killed) = reclaim_in(
        &journal,
        &World::orphaned("secode-plugin-sentinel-othernonce1"),
    );
    assert_eq!((report.stale.len(), killed.len()), (1, 0), "{report:?}");
    // A wrong parent or a start before the intent: that process may not be
    // signalled, but it carries the nonce, so the launch is not proven ended
    // and the record is kept.
    for world in [
        World::default().with(SENTINEL, 100, 55, SENTINEL, Some(&tag())),
        World::default().with(SENTINEL, 99, 1, SENTINEL, Some(&tag())),
    ] {
        journal
            .publish(&intent)
            .unwrap_or_else(|error| panic!("publish: {error}"));
        let (report, killed) = reclaim_in(&journal, &world);
        assert_eq!(
            (report.unverified.len(), killed.len()),
            (1, 0),
            "{report:?}"
        );
        assert_eq!(on_disk(&journal).len(), 1);
    }
    let proof = prove_ended(&intent, &World::default())
        .unwrap_or_else(|| panic!("nothing carries the nonce, so the launch ended"));
    journal
        .retire_record(proof)
        .unwrap_or_else(|error| panic!("retire: {error}"));
    // Two candidates, or an unreadable process of the user: preserved.
    let two = World::orphaned(&tag()).with(900, 100, 1, 900, Some(&tag()));
    let unreadable = World {
        tags_unreadable: true,
        ..World::orphaned(&tag())
    };
    for world in [two, unreadable] {
        journal
            .publish(&intent)
            .unwrap_or_else(|error| panic!("publish: {error}"));
        let (report, killed) = reclaim_in(&journal, &world);
        assert_eq!(
            (report.unverified.len(), killed.len()),
            (1, 0),
            "{report:?}"
        );
        let proof = prove_ended(&intent, &World::default())
            .unwrap_or_else(|| panic!("nothing carries the nonce, so the launch ended"));
        journal
            .retire_record(proof)
            .unwrap_or_else(|error| panic!("retire: {error}"));
    }
}

#[test]
fn a_gone_sentinel_is_stale_only_once_its_group_is_empty() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    journal
        .publish(&record(uid, recorded_sentinel()))
        .unwrap_or_else(|error| panic!("publish: {error}"));
    let unpinned = World::default().with(BRIDGE, 101, 1, SENTINEL, None);
    let (report, killed) = reclaim_in(&journal, &unpinned);
    assert_eq!(
        (report.unverified.len(), killed.len()),
        (1, 0),
        "{report:?}"
    );
    // Reported as unpinned members, not merely as an unproven stale record.
    assert!(
        report.unverified[0].contains("1 unpinned member(s) remain"),
        "{report:?}"
    );
    // Exited but unreaped is not yet stale: kept, unsignalled.
    let zombie = World {
        unreaped: vec![SENTINEL],
        ..World::default()
    };
    let (report, killed) = reclaim_in(&journal, &zombie);
    assert_eq!(
        (report.unverified.len(), killed.len()),
        (1, 0),
        "{report:?}"
    );
    assert!(
        report.unverified[0].contains("not yet reaped"),
        "{report:?}"
    );
    let (report, killed) = reclaim_in(&journal, &World::default());
    assert_eq!((report.stale.len(), killed.len()), (1, 0), "{report:?}");
    assert!(on_disk(&journal).is_empty());
}

#[test]
fn a_group_that_does_not_end_within_the_bound_keeps_its_record() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    journal
        .publish(&record(uid, recorded_sentinel()))
        .unwrap_or_else(|error| panic!("publish: {error}"));
    let world = World {
        stubborn: true,
        ..World::orphaned(&tag())
    };
    let started = tokio::time::Instant::now();
    let (report, killed) = reclaim_in(&journal, &world);
    assert!(tokio::time::Instant::now().duration_since(started) < REAP + Duration::from_secs(1));
    assert_eq!(
        (report.unverified.len(), killed.len()),
        (1, 1),
        "{report:?}"
    );
    assert_eq!(on_disk(&journal).len(), 1);
}

#[test]
fn a_concurrent_reclaim_finds_the_journal_claimed_and_signals_nothing() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    journal
        .publish(&record(uid, recorded_sentinel()))
        .unwrap_or_else(|error| panic!("publish: {error}"));
    let held = journal
        .claim()
        .unwrap_or_else(|error| panic!("claim: {error}"))
        .unwrap_or_else(|| panic!("the first claim was refused"));
    let (report, killed) = reclaim_in(&journal, &World::orphaned(&tag()));
    assert!(report.busy && killed.is_empty(), "{report:?}");
    drop(held);
    let (report, _) = reclaim_in(&journal, &World::orphaned(&tag()));
    assert_eq!(report.reclaimed.len(), 1);
}

/// A launcher whose steps can fail, recording what it spawned.
#[derive(Default)]
#[cfg_attr(
    not(windows),
    expect(clippy::struct_excessive_bools, reason = "legacy")
)]
struct Steps {
    sentinel_fails: bool,
    /// The sentinel step fails without confirming nothing was left.
    sentinel_survives: bool,
    bridge_fails: bool,
    abandon_fails: bool,
    spawned: Vec<&'static str>,
    abandoned: usize,
}

impl Launcher for Steps {
    type Sentinel = u32;
    type Bridge = u32;

    fn sentinel(&mut self, tag: &str) -> Result<(u32, Recorded), NotLaunched> {
        assert!(tag.starts_with(SENTINEL_TAG));
        if self.sentinel_fails || self.sentinel_survives {
            return Err(NotLaunched {
                error: PluginError::Spawn("sentinel refused".to_owned()),
                nothing_left: !self.sentinel_survives,
            });
        }
        self.spawned.push("sentinel");
        Ok((
            SENTINEL,
            Recorded {
                pid: SENTINEL,
                started: 100,
            },
        ))
    }

    fn bridge(&mut self, group: u32) -> Result<u32, PluginError> {
        assert_eq!(group, SENTINEL, "the bridge joins the sentinel's group");
        if self.bridge_fails {
            return Err(PluginError::Spawn("bridge refused".to_owned()));
        }
        self.spawned.push("bridge");
        Ok(BRIDGE)
    }

    fn abandon(&mut self, _: u32) -> Result<(), PluginError> {
        self.abandoned += 1;
        if self.abandon_fails {
            Err(PluginError::SurvivesKill("sentinel survived".to_owned()))
        } else {
            Ok(())
        }
    }
}

#[test]
fn launch_publishes_before_each_spawn_and_cleans_up_every_failed_step() {
    // Success: the record names the sentinel before the bridge exists.
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    let mut steps = Steps::default();
    let launched = launch(&journal, &mut steps, &World::default(), record(uid, None));
    assert!(launched.is_ok());
    assert_eq!(steps.spawned, vec!["sentinel", "bridge"]);
    assert_eq!(
        on_disk(&journal),
        vec![Scanned::Record(record(uid, recorded_sentinel()))]
    );
    // The sentinel fails: nothing spawned, and the intent is gone.
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    let mut steps = Steps {
        sentinel_fails: true,
        ..Steps::default()
    };
    assert!(launch(&journal, &mut steps, &World::default(), record(uid, None)).is_err());
    assert!(steps.spawned.is_empty() && on_disk(&journal).is_empty());
    // The bridge fails: the sentinel is abandoned, then its record removed.
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    let mut steps = Steps {
        bridge_fails: true,
        ..Steps::default()
    };
    assert!(launch(&journal, &mut steps, &World::default(), record(uid, None)).is_err());
    assert_eq!((steps.abandoned, on_disk(&journal).len()), (1, 0));
    // An abandon that cannot be confirmed keeps the record for a successor.
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    let mut steps = Steps {
        bridge_fails: true,
        abandon_fails: true,
        ..Steps::default()
    };
    assert!(launch(&journal, &mut steps, &World::default(), record(uid, None)).is_err());
    assert_eq!(
        on_disk(&journal),
        vec![Scanned::Record(record(uid, recorded_sentinel()))]
    );
}

#[test]
fn a_failed_sentinel_publication_never_spawns_the_bridge() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    let dir = scratch.0.join("plugin-recovery");
    /// Publishes the intent, then makes the journal unwritable before the
    /// sentinel's identity can be published.
    struct Locking<'a> {
        steps: Steps,
        dir: &'a Path,
    }
    impl Launcher for Locking<'_> {
        type Sentinel = u32;
        type Bridge = u32;
        fn sentinel(&mut self, tag: &str) -> Result<(u32, Recorded), NotLaunched> {
            let spawned = self.steps.sentinel(tag);
            std::fs::set_permissions(self.dir, std::fs::Permissions::from_mode(0o500)).map_err(
                |error| NotLaunched {
                    error: PluginError::Runtime(error.to_string()),
                    nothing_left: false,
                },
            )?;
            spawned
        }
        fn bridge(&mut self, group: u32) -> Result<u32, PluginError> {
            self.steps.bridge(group)
        }
        fn abandon(&mut self, sentinel: u32) -> Result<(), PluginError> {
            self.steps.abandon(sentinel)
        }
    }
    let mut locking = Locking {
        steps: Steps::default(),
        dir: &dir,
    };
    assert!(launch(&journal, &mut locking, &World::default(), record(uid, None)).is_err());
    assert_eq!(
        locking.steps.spawned,
        vec!["sentinel"],
        "the bridge was spawned"
    );
    assert_eq!(locking.steps.abandoned, 1);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .unwrap_or_else(|error| panic!("chmod: {error}"));
    // Only the intent survived the failed publication; with the sentinel
    // abandoned, a successor finds nothing carrying its nonce: stale.
    assert_eq!(on_disk(&journal), vec![Scanned::Record(record(uid, None))]);
    let (report, killed) = reclaim_in(&journal, &World::default());
    assert_eq!((report.stale.len(), killed.len()), (1, 0));
}

#[test]
fn every_crash_cut_leaves_a_record_that_reclaims_whatever_was_spawned() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    // After the intent, before the sentinel: nothing to signal, stale.
    journal
        .publish(&record(uid, None))
        .unwrap_or_else(|error| panic!("publish: {error}"));
    let (report, killed) = reclaim_in(&journal, &World::default());
    assert_eq!((report.stale.len(), killed.len()), (1, 0));
    // After the sentinel, before its record: found by nonce, reclaimed.
    journal
        .publish(&record(uid, None))
        .unwrap_or_else(|error| panic!("publish: {error}"));
    let sentinel_only = World::default().with(SENTINEL, 100, 1, SENTINEL, Some(&tag()));
    let (report, killed) = reclaim_in(&journal, &sentinel_only);
    assert_eq!((report.reclaimed.len(), killed.len()), (1, 1));
    // After the sentinel's record, and after the bridge: verified, reclaimed.
    for world in [sentinel_only.clone(), World::orphaned(&tag())] {
        let world = World {
            killed: Rc::new(RefCell::new(Vec::new())),
            ..world
        };
        journal
            .publish(&record(uid, recorded_sentinel()))
            .unwrap_or_else(|error| panic!("publish: {error}"));
        let (report, killed) = reclaim_in(&journal, &world);
        assert_eq!((report.reclaimed.len(), killed.len()), (1, 1), "{report:?}");
        assert!(on_disk(&journal).is_empty());
    }
}

/// Ruling condition 4: the gate re-reads every reclaim condition just before
/// its one group signal, not from the record scan.
#[test]
fn the_gate_refuses_a_reclaim_whose_sentinel_changed_after_the_decision() {
    let decided = record(0, recorded_sentinel());
    let before = World::orphaned(&tag());
    let Decision::Reclaim(target) = decide(&decided, &before, OWN) else {
        panic!("the orphan did not verify")
    };
    for after in [
        // The sentinel's PID now runs another process.
        World::default().with(SENTINEL, 999, 1, SENTINEL, Some(&tag())),
        // It no longer leads the group.
        World::default().with(SENTINEL, 100, 1, 900, Some(&tag())),
        // Someone else adopted it.
        World::default().with(SENTINEL, 100, 55, SENTINEL, Some(&tag())),
    ] {
        let mut kills = Kills(Rc::clone(&after.killed));
        let refused = signal::signal(Target::RecordedGroup(target), OWN, &after, &mut kills);
        assert_eq!(refused, Err(signal::Refusal::Unverified(SENTINEL)));
        assert!(
            after.killed.borrow().is_empty(),
            "a changed sentinel's group was signalled"
        );
    }
    let mut kills = Kills(Rc::clone(&before.killed));
    assert!(signal::signal(Target::RecordedGroup(target), OWN, &before, &mut kills).is_ok());
}

#[test]
fn a_sentinel_step_that_may_have_left_a_survivor_keeps_its_intent() {
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    let mut steps = Steps {
        sentinel_survives: true,
        ..Steps::default()
    };
    assert!(launch(&journal, &mut steps, &World::default(), record(uid, None)).is_err());
    assert!(steps.spawned.is_empty());
    // The intent stays, so a successor can find the survivor by its nonce.
    assert_eq!(on_disk(&journal), vec![Scanned::Record(record(uid, None))]);
}

#[test]
fn an_unreadable_nonce_candidate_or_an_impossible_time_is_preserved_unverified() {
    let mut unreadable = World::default();
    unreadable.procs.insert(
        SENTINEL,
        Proc {
            seen: Seen::Unreadable,
            group: SENTINEL,
            tag: Some(tag()),
        },
    );
    let decided = decide(&record(0, None), &unreadable, OWN);
    assert!(matches!(decided, Decision::Unverified(_)), "{decided:?}");
    for intent_millis in [i64::MIN, -1] {
        let impossible = Record {
            intent_millis,
            ..record(0, None)
        };
        let decided = decide(&impossible, &World::orphaned(&tag()), OWN);
        assert!(matches!(decided, Decision::Unverified(_)), "{decided:?}");
    }
    // A start time past any representable millisecond never fits, never panics.
    let late = World::default().with(SENTINEL, u64::MAX, 1, SENTINEL, Some(&tag()));
    let intent = Record {
        intent_millis: i64::MAX,
        ..record(0, None)
    };
    assert_eq!(decide(&intent, &late, OWN), Decision::Stale);
}

#[test]
fn the_gate_refuses_a_reclaim_whose_owner_is_back_or_unreadable() {
    let decided = record(0, recorded_sentinel());
    let Decision::Reclaim(target) = decide(&decided, &World::orphaned(&tag()), OWN) else {
        panic!("the orphan did not verify")
    };
    let back = World::orphaned(&tag()).with(OWNER, 50, 1, OWNER, None);
    let mut unreadable = World::orphaned(&tag());
    unreadable.procs.insert(
        OWNER,
        Proc {
            seen: Seen::Unreadable,
            group: OWNER,
            tag: None,
        },
    );
    for after in [back, unreadable] {
        let mut kills = Kills(Rc::clone(&after.killed));
        let refused = signal::signal(Target::RecordedGroup(target), OWN, &after, &mut kills);
        assert_eq!(refused, Err(signal::Refusal::Unverified(SENTINEL)));
        assert!(after.killed.borrow().is_empty());
    }
}

#[test]
fn a_publication_that_fails_at_any_durable_step_never_spawns_and_leaves_no_temporary() {
    for fault in [Fault::Write, Fault::Sync, Fault::Rename, Fault::DirSync] {
        let (scratch, uid) = new_scratch();
        let journal = open_journal(&scratch, uid);
        journal.inject(Some(fault));
        let mut steps = Steps::default();
        assert!(
            launch(&journal, &mut steps, &World::default(), record(uid, None)).is_err(),
            "{fault:?}"
        );
        assert!(
            steps.spawned.is_empty(),
            "{fault:?} spawned {:?}",
            steps.spawned
        );
        let leftovers: Vec<String> = std::fs::read_dir(scratch.0.join("plugin-recovery"))
            .unwrap_or_else(|error| panic!("read: {error}"))
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{fault:?} left {leftovers:?}");
        // At most the unspawned intent itself remains, and it is stale.
        journal.inject(None);
        for scanned in on_disk(&journal) {
            assert_eq!(scanned, Scanned::Record(record(uid, None)), "{fault:?}");
        }
    }
}

#[test]
fn an_incomplete_census_never_reads_as_an_empty_group() {
    let reads = [
        (1, GroupRead::In(1)),
        (10, GroupRead::In(800)),
        (11, GroupRead::Gone),
    ];
    assert_eq!(census_members(800, reads), Some(vec![10]));
    assert_eq!(census_members(900, reads), Some(vec![]));
    let uncertain = [(10, GroupRead::Gone), (12, GroupRead::Unreadable)];
    assert_eq!(census_members(800, uncertain), None);
}

#[test]
fn a_failed_parent_sync_is_retried_on_every_open() {
    let (scratch, uid) = new_scratch();
    let dir = scratch.0.join("plugin-recovery");
    assert!(Journal::open_with(&dir, uid, true).is_err());
    assert!(
        dir.is_dir(),
        "the directory was created before its sync failed"
    );
    // The directory now exists: a retry must still sync its parent.
    assert!(Journal::open_with(&dir, uid, true).is_err());
    assert!(Journal::open(&dir, uid).is_ok());
}

/// The structural rule: a record goes only on a fresh census proof that
/// its launch ended. Every other state yields no proof, so the record stays.
#[test]
fn only_a_fresh_proof_of_an_ended_launch_retires_a_record() {
    let launched = record(0, recorded_sentinel());
    let unreadable = |pid: u32, group: u32| {
        let mut world = World::default();
        world.procs.insert(
            pid,
            Proc {
                seen: Seen::Unreadable,
                group,
                tag: None,
            },
        );
        world
    };
    let unknown_members = World {
        members_unknown: true,
        ..World::default()
    };
    for (why, world) in [
        ("the sentinel is alive", World::orphaned(&tag())),
        // Its group read elsewhere, so only the sentinel's read is unknown.
        ("the sentinel is unreadable", unreadable(SENTINEL, 900)),
        (
            "a member is left",
            World::default().with(BRIDGE, 101, 1, SENTINEL, None),
        ),
        ("the members are unknown", unknown_members.clone()),
        // Darwin's table drops a zombie the kernel still holds.
        (
            "the sentinel exited but is not reaped",
            World {
                unreaped: vec![SENTINEL],
                ..World::default()
            },
        ),
    ] {
        assert!(prove_ended(&launched, &world).is_none(), "{why}");
    }
    let intent = record(0, None);
    let tags_unknown = World {
        tags_unreadable: true,
        ..World::default()
    };
    for (why, world) in [
        ("a process carries the nonce", World::orphaned(&tag())),
        ("the user's processes are unreadable", tags_unknown),
    ] {
        assert!(prove_ended(&intent, &world).is_none(), "{why}");
    }
    // A reused sentinel PID with an empty group, or nothing at all: ended.
    let reused = World::default().with(SENTINEL, 999, 1, 900, None);
    assert!(prove_ended(&launched, &reused).is_some());
    assert!(prove_ended(&intent, &World::default()).is_some());
}

#[test]
fn every_failed_cleanup_path_keeps_the_record() {
    // A survivor of the sentinel step, an unconfirmed abandon, and a sentinel
    // still alive when the bridge fails: each leaves the record on disk.
    let cases: [(Steps, World); 3] = [
        (
            Steps {
                sentinel_survives: true,
                ..Steps::default()
            },
            World::default(),
        ),
        (
            Steps {
                bridge_fails: true,
                abandon_fails: true,
                ..Steps::default()
            },
            World::default(),
        ),
        (
            Steps {
                bridge_fails: true,
                ..Steps::default()
            },
            // The abandon "succeeded", but the census still sees the sentinel.
            World::default().with(SENTINEL, 100, 4000, SENTINEL, Some(&tag())),
        ),
    ];
    for (mut steps, world) in cases {
        let (scratch, uid) = new_scratch();
        let journal = open_journal(&scratch, uid);
        assert!(launch(&journal, &mut steps, &world, record(uid, None)).is_err());
        assert_eq!(
            on_disk(&journal).len(),
            1,
            "a record was lost on a failed cleanup"
        );
    }
    // Unknown members also keep it through reclaim.
    let (scratch, uid) = new_scratch();
    let journal = open_journal(&scratch, uid);
    journal
        .publish(&record(uid, recorded_sentinel()))
        .unwrap_or_else(|error| panic!("publish: {error}"));
    let unknown = World {
        members_unknown: true,
        ..World::default()
    };
    let (report, killed) = reclaim_in(&journal, &unknown);
    assert_eq!(
        (report.unverified.len(), killed.len()),
        (1, 0),
        "{report:?}"
    );
    assert_eq!(on_disk(&journal).len(), 1);
}

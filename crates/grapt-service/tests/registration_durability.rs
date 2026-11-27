//! R4D-2C acceptance: the durable half, against a real filesystem.
//!
//! `controller_contract.rs` proves the transitions over a deterministic double.
//! That double cannot prove the two properties the master actually requires of
//! the *store*: that registration mutation happens *"under an OS/file lock"*,
//! and that a compare-and-swap against registration-file **contents** is
//! indivisible. A fake lock proves only that the fake is consistent with
//! itself.
//!
//! So everything here uses a real file, a real `std::fs::File` lock, and — for
//! the race — real threads. The properties witnessed are:
//!
//! * two starters racing through the real lock produce exactly one winner;
//! * an interleaving that would defeat a check-then-act CAS still produces one
//!   winner, because the compare is inside the lock rather than beside it;
//! * a write lands atomically, so no crash cut leaves a torn registration;
//! * bytes corrupted on disk decode as unreadable and never as a different
//!   owner.
//!
//! # Temporary directories
//!
//! Each test gets its own directory under the system temp dir, named from the
//! process id and a per-process counter, and removed first in case a previous
//! run with a recycled process id left one behind. That naming is explicitly
//! *not* good enough for a durable identity — `clippy.toml` records why a
//! `{process id}-{counter}` writer identity was a real defect — but it is
//! exactly right for a scratch directory whose only requirement is that two
//! concurrently running tests do not collide.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use grapt_service::{
    ElectedControllerAuthority, LockAttemptPolicy, Observation, ProbeOutcome, RegistrationRecord,
    Retirement, ServiceController, SlotState, StopOutcome, StoreError, MAX_REGISTRATION_BYTES,
};
use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ReplacementOutcome, ServiceGeneration,
    ServiceInstanceId, ServiceRegistrationVersion, ServiceSlotId,
};

type R = Result<(), Box<dyn std::error::Error>>;

static SCRATCH: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

const SLOT: &str = "slot-secode";
const ENVIRONMENT: &str = "env-alpha";

/// A scratch directory that removes itself.
#[derive(Debug)]
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let ordinal = SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "grapt-service-sv-{}-{ordinal}-{name}",
            std::process::id()
        ));
        // Removed first: a recycled process id could otherwise hand this test a
        // directory a previous run left behind.
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn slot_path(&self) -> PathBuf {
        self.root.join("registration")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn owner(instance: &str, generation: u64, pid: u32, port: u16) -> Option<OwnerReceipt> {
    Some(OwnerReceipt::new(
        ServiceSlotId::new(SLOT)?,
        ServiceInstanceId::new(instance)?,
        EnvironmentReceipt::new(ENVIRONMENT)?,
        ServiceGeneration::new(generation),
        ProcessId::new(pid)?,
        Endpoint::new(format!("http://127.0.0.1:{port}"))?,
    ))
}

fn observation(declared: OwnerReceipt) -> Option<Observation> {
    Some(Observation::new(
        declared.clone(),
        ProbeOutcome::Attributed(declared),
        ServiceRegistrationVersion::new("0.1.0")?,
    ))
}

fn record(owner: OwnerReceipt, at: i64) -> Option<RegistrationRecord> {
    RegistrationRecord::new(
        owner,
        ServiceRegistrationVersion::new("0.1.0")?,
        Timestamp::from_millis(at),
    )
    .ok()
}

fn authority(path: PathBuf) -> Result<ElectedControllerAuthority, Box<dyn std::error::Error>> {
    ElectedControllerAuthority::acquire(
        path,
        LockAttemptPolicy::new(1_000_000).ok_or("nonzero lock policy")?,
    )
    .map_err(Into::into)
}

fn registration_path(
    authority: &ElectedControllerAuthority,
) -> Result<&std::path::Path, Box<dyn std::error::Error>> {
    authority
        .registration_path()
        .ok_or_else(|| "file-backed authority has no registration path".into())
}

fn new_controller<'a>(
    store: &'a ElectedControllerAuthority,
    clock: &'a FixedClock,
) -> Result<ServiceController<'a>, Box<dyn std::error::Error>> {
    Ok(ServiceController::new(
        ServiceSlotId::new(SLOT).ok_or("slot")?,
        EnvironmentReceipt::new(ENVIRONMENT).ok_or("environment")?,
        store,
        clock,
    ))
}

#[test]
fn the_lock_path_is_derived_from_the_registration_rather_than_supplied() -> R {
    // Two controllers pointed at one registration through two different lock
    // files would each hold a lock nobody else respects. Deriving the path is
    // what makes that unrepresentable, so it is worth asserting directly.
    let scratch = Scratch::new("derived-lock")?;
    let store = authority(scratch.slot_path())?;
    let expected = {
        let mut raw = scratch.slot_path().into_os_string();
        raw.push(".lock");
        PathBuf::from(raw)
    };
    assert_eq!(
        store.lock_path().ok_or("lock path")?,
        expected.as_path(),
        "the lock must be a derived sibling of the registration, so two stores over one slot \
         cannot end up holding two different locks"
    );
    assert_ne!(
        store.lock_path().ok_or("lock path")?,
        registration_path(&store)?,
        "the lock must not be the registration itself, because releasing a slot unlinks it"
    );
    Ok(())
}

#[test]
fn a_registration_written_through_the_file_store_round_trips_on_disk() -> R {
    let scratch = Scratch::new("round-trip")?;
    let store = authority(scratch.slot_path())?;
    let clock = FixedClock(1_700_000_000_000);
    let controller = new_controller(&store, &clock)?;

    let candidate = observation(owner("inst-a", 0, 4242, 8080).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let discovered = controller.discover()?;
    let prepared = controller.prepare(&discovered, &candidate)?;
    let receipt = controller.commit(prepared, Retirement::NoIncumbent)?;
    assert_eq!(
        receipt.outcome(),
        ReplacementOutcome::Replaced,
        "claiming an empty slot on disk must commit"
    );

    // Read the file back through plain filesystem access, not through the
    // store, so the assertion is about what is actually on disk.
    let raw = std::fs::read(registration_path(&store)?)?;
    let decoded = RegistrationRecord::decode(&raw)?;
    let committed = receipt.committed_owner().ok_or("committed owner")?;
    assert!(
        decoded.owner().is_same_owner(committed),
        "the bytes on disk must name the owner the switch committed"
    );
    assert_eq!(
        decoded.encode(),
        raw,
        "the file's contents must be exactly what this encoder produces, or a contents \
         compare-and-swap is comparing something other than the record"
    );
    assert!(
        !raw.contains(&b'\r'),
        "the registration must be written without carriage returns: one would make the \
         incumbent's own record stop matching itself under a byte compare"
    );
    Ok(())
}

#[test]
fn two_threads_racing_through_the_real_lock_produce_exactly_one_winner() -> R {
    let scratch = Scratch::new("race")?;
    let store = authority(scratch.slot_path())?;
    let clock = FixedClock(1_700_000_000_000);

    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let seeded = record(incumbent, 1_000).ok_or("incumbent record")?;
    std::fs::write(registration_path(&store)?, seeded.encode())?;

    // Both starters read the incumbent's bytes BEFORE either commits, then
    // commit concurrently. Only the compare inside the lock can separate them.
    let controller = new_controller(&store, &clock)?;
    let first_view = controller.discover()?;
    let second_view = controller.discover()?;
    drop(controller);

    let candidates = [
        ("inst-b", 5353u32, 8081u16, first_view),
        ("inst-c", 6464, 8082, second_view),
    ];

    let outcomes = std::thread::scope(
        |scope| -> Result<Vec<ReplacementOutcome>, Box<dyn std::error::Error>> {
            let mut handles = Vec::new();
            for (instance, pid, port, view) in candidates {
                let store = &store;
                let clock = &clock;
                handles.push(scope.spawn(move || -> Result<ReplacementOutcome, String> {
                    // The controller is built INSIDE the thread so that only the
                    // concrete store and clock cross the boundary.
                    let controller = ServiceController::new(
                        ServiceSlotId::new(SLOT).ok_or("slot")?,
                        EnvironmentReceipt::new(ENVIRONMENT).ok_or("environment")?,
                        store,
                        clock,
                    );
                    let declared = owner(instance, 2, pid, port).ok_or("candidate")?;
                    let candidate = observation(declared).ok_or("observation")?;
                    let prepared = controller
                        .prepare(&view, &candidate)
                        .map_err(|error| error.to_string())?;
                    let receipt = controller
                        .commit(prepared, Retirement::Retired)
                        .map_err(|error| error.to_string())?;
                    Ok(receipt.outcome())
                }));
            }
            let mut outcomes = Vec::new();
            for handle in handles {
                match handle.join() {
                    Ok(result) => outcomes.push(
                        result.map_err(|error| -> Box<dyn std::error::Error> { error.into() })?,
                    ),
                    Err(_) => return Err("a racing starter panicked".into()),
                }
            }
            Ok(outcomes)
        },
    )?;

    let winners = outcomes
        .iter()
        .filter(|outcome| **outcome == ReplacementOutcome::Replaced)
        .count();
    let losers = outcomes
        .iter()
        .filter(|outcome| **outcome == ReplacementOutcome::LostRace)
        .count();
    assert_eq!(
        (winners, losers),
        (1, 1),
        "two concurrent starters against one slot must settle as exactly one replacement and one \
         lost race; anything else means both wrote or neither did"
    );

    // And the slot is left holding one complete, readable registration.
    let raw = std::fs::read(registration_path(&store)?)?;
    let decoded = RegistrationRecord::decode(&raw)?;
    assert_eq!(
        decoded.owner().generation().value(),
        2,
        "the surviving registration must be the successor generation exactly once, never twice \
         applied or half written"
    );
    Ok(())
}

#[test]
fn a_lost_compare_against_the_real_store_writes_nothing() -> R {
    let scratch = Scratch::new("lost-compare")?;
    let store = authority(scratch.slot_path())?;
    let clock = FixedClock(10);
    let controller = new_controller(&store, &clock)?;
    let view = controller.discover()?;
    let candidate =
        observation(owner("inst-a", 0, 7, 8080).ok_or("candidate")?).ok_or("observation")?;
    let prepared = controller.prepare(&view, &candidate)?;
    std::fs::write(registration_path(&store)?, b"actual contents\n")?;
    let receipt = controller.commit(prepared, Retirement::NoIncumbent)?;
    assert_eq!(receipt.outcome(), ReplacementOutcome::LostRace);
    assert_eq!(receipt.observed(), Some(b"actual contents\n".as_slice()));
    assert_eq!(
        std::fs::read(registration_path(&store)?)?,
        b"actual contents\n",
        "a lost compare must leave the file byte-identical on disk"
    );
    Ok(())
}

#[test]
fn claiming_and_releasing_a_slot_round_trips_through_absence() -> R {
    let scratch = Scratch::new("claim-release")?;
    let store = authority(scratch.slot_path())?;
    let clock = FixedClock(10);
    let controller = new_controller(&store, &clock)?;

    assert_eq!(
        store.read_registration()?,
        None,
        "a slot whose file does not exist reads as absent rather than as an error"
    );
    let candidate_owner = owner("inst-a", 0, 7, 8080).ok_or("candidate")?;
    let candidate = observation(candidate_owner).ok_or("observation")?;
    let prepared = controller.prepare(&controller.discover()?, &candidate)?;
    let receipt = controller.commit(prepared, Retirement::NoIncumbent)?;
    let committed = receipt.committed_owner().ok_or("committed owner")?.clone();
    assert_eq!(
        controller.discover()?.registered_owner(),
        Some(&committed),
        "a claimed slot must read back the committed owner"
    );
    let discovered = controller.discover()?;
    assert!(matches!(
        controller.stop(&discovered, &committed)?,
        StopOutcome::Released { .. }
    ));
    assert_eq!(
        store.read_registration()?,
        None,
        "a released slot must read as absent, not as an empty registration"
    );
    assert!(
        !registration_path(&store)?.exists(),
        "releasing a slot must remove the registration file rather than truncate it, because an \
         empty file would decode as a refusal instead of as an unclaimed slot"
    );
    Ok(())
}

#[test]
fn a_registration_corrupted_on_disk_is_unreadable_and_never_a_different_owner() -> R {
    let scratch = Scratch::new("corruption")?;
    let store = authority(scratch.slot_path())?;
    let clock = FixedClock(1_700_000_000_000);
    let controller = new_controller(&store, &clock)?;

    let good = record(owner("inst-a", 1, 4242, 8080).ok_or("owner")?, 1_000)
        .ok_or("record")?
        .encode();

    // Every truncation a partial write could leave behind, plus a CRLF
    // rewrite, plus outright garbage. Not one may present as an owner.
    let mut cases: Vec<(String, Vec<u8>)> = (0..good.len())
        .map(|cut| (format!("a {cut}-byte prefix"), good[..cut].to_vec()))
        .collect();
    cases.push((
        "a CRLF rewrite".to_owned(),
        String::from_utf8(good.clone())?
            .replace('\n', "\r\n")
            .into_bytes(),
    ));
    cases.push((
        "arbitrary bytes".to_owned(),
        b"\x00\xff not a record\n".to_vec(),
    ));

    for (name, bytes) in cases {
        std::fs::write(registration_path(&store)?, &bytes)?;
        let discovered = controller.discover()?;
        match discovered.state() {
            SlotState::Unreadable(_) => {}
            SlotState::Empty => {
                // The zero-byte prefix genuinely is an empty FILE, which is not
                // the same as an absent one — it must still not be an owner.
                if !bytes.is_empty() {
                    return Err(format!("{name} was read as an empty slot").into());
                }
            }
            SlotState::Registered(record) => {
                return Err(format!(
                    "{name} decoded as a live registration naming instance {}: a corrupted slot \
                     must never present as an owner nobody wrote",
                    record.owner().instance()
                )
                .into())
            }
        }
        assert!(
            discovered.registered_owner().is_none(),
            "{name} reported a registered owner"
        );
    }

    // The control: the intact record still reads as an owner. Without it, a
    // `discover` that returned `Unreadable` unconditionally would satisfy every
    // case above.
    std::fs::write(registration_path(&store)?, &good)?;
    let discovered = controller.discover()?;
    assert!(
        discovered.registered_owner().is_some(),
        "the intact registration must still read as an owner, or the corruption cases above pass \
         for the wrong reason"
    );
    Ok(())
}

#[test]
fn election_lock_contention_exhausts_the_declared_finite_attempt_budget() -> R {
    let scratch = Scratch::new("finite-election-budget")?;
    let path = scratch.slot_path();
    let _elected = authority(path.clone())?;
    let policy = LockAttemptPolicy::new(3).ok_or("policy")?;
    match ElectedControllerAuthority::acquire(path, policy) {
        Err(StoreError::LockBudgetExhausted { attempts: 3, .. }) => Ok(()),
        other => Err(format!(
            "contended election did not exhaust exactly three attempts: {other:?}"
        )
        .into()),
    }
}

#[test]
fn oversized_registration_is_refused_before_unbounded_read_allocation() -> R {
    let scratch = Scratch::new("bounded-read")?;
    let store = authority(scratch.slot_path())?;
    std::fs::write(
        registration_path(&store)?,
        vec![b'x'; MAX_REGISTRATION_BYTES + 1],
    )?;
    match store.read_registration() {
        Err(StoreError::RegistrationTooLarge { maximum, found, .. }) => {
            assert_eq!(maximum, MAX_REGISTRATION_BYTES);
            assert_eq!(found, u64::try_from(MAX_REGISTRATION_BYTES + 1)?);
            Ok(())
        }
        other => {
            Err(format!("oversized registration did not hit the read bound: {other:?}").into())
        }
    }
}

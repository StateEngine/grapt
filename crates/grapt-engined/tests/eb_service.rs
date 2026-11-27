//! EB09 and EB14 — what `service status` does, and what it says it saw.
//!
//! Two different laws over one verb, deliberately kept apart. EB09 is a
//! filesystem law: observing must create nothing. EB14 is a rendering law: the
//! receipt must carry what was observed. A candidate can satisfy either while
//! failing the other, and for one revision of this lane it did exactly that —
//! `run_status` computed a three-way state and an owner, and the binary printed
//! the constant `outcome=observed` for all three.
//!
//! This is a composition law rather than a domain law, and it is easy to get
//! wrong in exactly one way: the obvious route to a registration is a
//! `ServiceController`, and constructing one requires an
//! `ElectedControllerAuthority` whose `acquire` takes the election lock. A
//! `status` that built a controller would therefore create a `.elected` file
//! just to look at the slot — observing by mutating.
//!
//! The witness watches the filesystem rather than the return value, because
//! the effect is the thing being forbidden.

// The module is shared source compiled into this test binary as well as into
// the binary itself. Each witness file uses the part of it that carries its own
// law, so items belonging to a sibling witness are legitimately unused HERE.
#[allow(dead_code)]
#[path = "../src/service.rs"]
mod service;

use crate::receipt_scratch;

use grapt_service::{LockAttemptPolicy, RegistrationRecord};
use grapt_types::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceRegistrationVersion, ServiceSlotId, Timestamp,
};
use service::{run_status, ServiceOutcome};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The instance the registered witness expects to read back out of the receipt.
const WITNESS_INSTANCE: &str = "engined-status-witness-instance";

/// A real, decodable registration for the one state the corpus cannot create.
///
/// E1a refuses `service ensure`, so no lawful invocation of this binary can
/// write a registration — which is exactly why this witness encodes the record
/// through the crate that owns its byte contract instead of faking bytes. The
/// timestamp is a CONSTRUCTED value, never a clock read: `Timestamp::from_millis`
/// takes the number it is given, and reaching for the wall clock here would take
/// the very authority this lane refuses.
fn registration_bytes() -> Vec<u8> {
    let missing = |what: &str| -> ! { panic!("the witness value for {what} must be admitted") };
    let Some(slot) = ServiceSlotId::new("engined-status-witness-slot") else {
        missing("slot")
    };
    let Some(instance) = ServiceInstanceId::new(WITNESS_INSTANCE) else {
        missing("instance")
    };
    let Some(environment) = EnvironmentReceipt::new("engined-status-witness-env") else {
        missing("environment")
    };
    let Some(pid) = ProcessId::new(4321) else {
        missing("pid")
    };
    // An `Endpoint` refuses a bare host: a URL scheme separator is required,
    // because a host and port alone is an address rather than an endpoint.
    let Some(endpoint) = Endpoint::new("http://127.0.0.1:4321") else {
        missing("endpoint")
    };
    let Some(version) = ServiceRegistrationVersion::new("1.0.0") else {
        missing("version")
    };
    let owner = OwnerReceipt::new(
        slot,
        instance,
        environment,
        ServiceGeneration::new(1),
        pid,
        endpoint,
    );
    match RegistrationRecord::new(owner, version, Timestamp::from_millis(1_700_000_000_000)) {
        Ok(record) => record.encode(),
        Err(error) => panic!("the witness registration must build: {error:?}"),
    }
}

/// Run the real binary's `status` against one slot and hand back what a caller
/// sees.
fn status_stdout(registration: &Path, receipt_root: &Path) -> (Option<i32>, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_grapt-engined"))
        .args(["service", "status", "--registration"])
        .arg(registration)
        .args(["--lock-attempts", "3", "--receipt-root"])
        .arg(receipt_root)
        .output()
        .unwrap_or_else(|error| panic!("the binary under test must be runnable: {error}"));
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    )
}

/// A registration path in a fresh directory nothing else writes to. The first
/// element keeps the case directory alive; drop it after the run is done.
fn slot_path(case: &str) -> (receipt_scratch::tmp_guard::Dir, PathBuf) {
    let scratch = receipt_scratch::outside_temp_dir();
    let base = scratch.join("grapt-engined-eb-service").join(case);
    std::fs::create_dir_all(&base)
        .unwrap_or_else(|error| panic!("the case directory must be creatable: {error}"));
    (scratch, base.join("registration.json"))
}

fn policy() -> LockAttemptPolicy {
    LockAttemptPolicy::new(3).unwrap_or_else(|| panic!("3 is a positive lock-attempt budget"))
}

/// Every sidecar the mutating paths would create.
fn sidecars(registration: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for suffix in [".lock", ".elected"] {
        let mut name = registration.as_os_str().to_owned();
        name.push(suffix);
        paths.push(PathBuf::from(name));
    }
    paths
}

/// EB09. Reading an empty slot creates nothing at all — not the registration,
/// not the lock, not the election marker.
#[test]
fn status_reads_the_registration_and_never_writes_it() {
    let (_slot, registration) = slot_path("empty");

    let observed = run_status(&registration, policy())
        .unwrap_or_else(|refusal| panic!("observing an absent slot must not refuse: {refusal:?}"));

    match observed {
        ServiceOutcome::Observed {
            ref state,
            ref owner,
        } => {
            assert_eq!(state, "empty", "an absent registration observes as empty");
            assert!(owner.is_none(), "an empty slot names no owner");
        }
        other => panic!("status must observe, not transition; got {other:?}"),
    }

    assert!(
        !registration.exists(),
        "observing a slot must not create the registration file"
    );
    for sidecar in sidecars(&registration) {
        assert!(
            !sidecar.exists(),
            "status must not create {}; acquiring an authority to observe is the \
             mutation this row forbids",
            sidecar.display()
        );
    }
}

/// The direction guard: an existing slot is still read, and reading it still
/// writes nothing. Without this, the row above could be satisfied by a status
/// that did nothing at all.
#[test]
fn status_reads_an_existing_slot_without_taking_a_lock() {
    let (_slot, registration) = slot_path("occupied");
    std::fs::write(&registration, b"not a decodable registration record")
        .unwrap_or_else(|error| panic!("the case file must be writable: {error}"));

    let before = std::fs::read(&registration)
        .unwrap_or_else(|error| panic!("the case file must be readable: {error}"));

    let observed = run_status(&registration, policy()).unwrap_or_else(|refusal| {
        panic!("observing an undecodable slot must not refuse: {refusal:?}")
    });

    match observed {
        ServiceOutcome::Observed { ref state, .. } => {
            assert_eq!(
                state, "unreadable",
                "bytes that are not a registration record observe as unreadable, \
                 never as empty"
            );
        }
        other => panic!("status must observe, not transition; got {other:?}"),
    }

    let after = std::fs::read(&registration)
        .unwrap_or_else(|error| panic!("the case file must still be readable: {error}"));
    assert_eq!(before, after, "observing must not rewrite the slot");

    for sidecar in sidecars(&registration) {
        assert!(
            !sidecar.exists(),
            "status must not create {} for an occupied slot either",
            sidecar.display()
        );
    }
}

/// EB14. A successful `status` renders WHAT IT OBSERVED.
///
/// The row this witness guards is a rendering law, and the defect it exists to
/// catch is precise: `ServiceOutcome::label()` returns the constant `"observed"`
/// for every observation, so a receipt built from the label alone reports that a
/// status ran and nothing about the slot. An operator could not tell an absent
/// registration from a slot somebody else holds from undecodable bytes, and
/// could never learn the owner.
///
/// It drives the BINARY, because the defect lived in what the binary PRINTS
/// while `run_status`'s return value was already correct — a witness on the
/// return value (EB09's shape) passes straight through it.
#[test]
fn a_successful_status_receipt_names_the_state_it_observed() {
    let (_registered_slot, registered) = slot_path("registered");
    std::fs::write(&registered, registration_bytes())
        .unwrap_or_else(|error| panic!("the witness registration must be writable: {error}"));
    let root = registered
        .parent()
        .unwrap_or_else(|| panic!("the case directory must have a parent"));

    let (_absent_slot, absent_base) = slot_path("absent-for-render");
    let absent = absent_base.join("nothing-here.json");
    let (_unreadable_slot, unreadable) = slot_path("unreadable-for-render");
    std::fs::write(&unreadable, b"not a decodable registration record")
        .unwrap_or_else(|error| panic!("the case file must be writable: {error}"));

    let (registered_code, registered_out, registered_prose) = status_stdout(&registered, root);
    let (absent_code, absent_out, absent_prose) = status_stdout(&absent, root);
    let (unreadable_code, unreadable_out, unreadable_prose) = status_stdout(&unreadable, root);

    for (label, code) in [
        ("registered", registered_code),
        ("absent", absent_code),
        ("unreadable", unreadable_code),
    ] {
        assert_eq!(code, Some(0), "a successful {label} status exits 0");
    }

    // THE KILLING ASSERTIONS. All three states are named, and the owner of the
    // held slot reaches the receipt.
    assert!(
        registered_out.contains("\"state\":\"registered\""),
        "a decodable registration must render as registered; got {registered_out}"
    );
    assert!(
        registered_out.contains(&format!("\"owner\":\"{WITNESS_INSTANCE}\"")),
        "the receipt must name the instance that holds the slot; got {registered_out}"
    );
    assert!(
        absent_out.contains("\"state\":\"empty\""),
        "an absent registration must render as empty; got {absent_out}"
    );
    assert!(
        unreadable_out.contains("\"state\":\"unreadable\""),
        "undecodable bytes must render as unreadable, never as empty; got {unreadable_out}"
    );

    // The three are mutually distinct. Without this, a rendering that collapsed
    // two states into one spelling could satisfy every assertion above.
    assert_ne!(
        absent_out, unreadable_out,
        "an empty slot and an undecodable one must not render alike"
    );
    assert_ne!(
        registered_out, absent_out,
        "a held slot and an empty one must not render alike"
    );
    assert_ne!(
        registered_out, unreadable_out,
        "a held slot and an undecodable one must not render alike"
    );

    // An ABSENT owner is an absent member, never an empty string: "" is a name
    // no owner can have and a value every consumer would have to special-case.
    for (label, receipt) in [("absent", &absent_out), ("unreadable", &unreadable_out)] {
        assert!(
            !receipt.contains("\"owner\""),
            "a {label} slot names no owner at all rather than an empty one; got {receipt}"
        );
    }

    // stdout and stderr must not disagree about one observation.
    for (state, prose) in [
        ("registered", &registered_prose),
        ("empty", &absent_prose),
        ("unreadable", &unreadable_prose),
    ] {
        assert!(
            prose.contains(state),
            "the prose companion must name the same state as the receipt; got {prose}"
        );
    }
    assert!(
        registered_prose.contains(WITNESS_INSTANCE),
        "the prose must name the owner the receipt names; got {registered_prose}"
    );
}

//! EB10 and EB11 — where a receipt is allowed to land.
//!
//! EB10: precedence is explicit argument, then `GRAPT_RECEIPT_ROOT`, then a
//! portable fallback. EB11: a receipt root inside a repository is refused.
//!
//! Declared by path for the reason given in `eb_platform.rs`.

// The module is shared source compiled into this test binary as well as into
// the binary itself. Each witness file uses the part of it that carries its own
// law, so items belonging to a sibling witness are legitimately unused HERE.
#[allow(dead_code)]
#[path = "../src/custody.rs"]
mod custody;

use crate::receipt_scratch;

use custody::{is_inside_repository, receipt_root, CustodyRefusal};
use std::path::PathBuf;

/// A directory that is not inside any repository, for the cases that must pass.
fn outside_root() -> receipt_scratch::tmp_guard::Dir {
    receipt_scratch::outside_temp_dir()
}

/// EB10. An operator who names a root on the command line has said something
/// more specific than a variable inherited from a shell three layers up. If the
/// environment outranked the argument, the flag would be a suggestion.
#[test]
fn an_explicit_receipt_root_outranks_the_environment() {
    let explicit_scratch = outside_root();
    let from_env_scratch = outside_root();
    let explicit = explicit_scratch.join("explicit");
    let from_env = from_env_scratch.join("environment");

    let settled = receipt_root(Some(explicit.clone()), Some(from_env.clone()), || {
        panic!("the fallback must not be reached when a root was supplied")
    })
    .unwrap_or_else(|refusal| panic!("an outside-repository root must settle: {refusal:?}"));

    assert_eq!(
        settled, explicit,
        "the explicit argument outranks GRAPT_RECEIPT_ROOT"
    );
    assert_ne!(
        settled, from_env,
        "the environment must never win over an explicit argument"
    );
}

/// The rest of the precedence chain, so EB10 cannot be satisfied by ignoring
/// the environment altogether.
#[test]
fn the_environment_is_used_only_when_no_argument_was_supplied() {
    let from_env_scratch = outside_root();
    let from_env = from_env_scratch.join("environment");
    let settled = receipt_root(None, Some(from_env.clone()), || {
        panic!("the fallback must not be reached when the environment supplied a root")
    })
    .unwrap_or_else(|refusal| panic!("an outside-repository root must settle: {refusal:?}"));
    assert_eq!(settled, from_env);

    let fallback_scratch = outside_root();
    let fallback = fallback_scratch.join("fallback");
    let settled = receipt_root(None, None, || Some(fallback.clone()))
        .unwrap_or_else(|refusal| panic!("the fallback must settle: {refusal:?}"));
    assert_eq!(settled, fallback, "the fallback is last, not first");

    match receipt_root(None, None, || None) {
        Err(CustodyRefusal::NoPortableReceiptRoot) => {}
        other => panic!("with no root and no fallback the settle must refuse; got {other:?}"),
    }
}

/// EB11. Writing a receipt into a candidate tree voids the run that produced
/// it, and the direct-include inventory counts gitignored directories, so a
/// stray file under a repository root reddens a gate on an otherwise clean tree.
#[test]
fn a_receipt_root_inside_a_repository_is_refused() {
    // This test file is itself inside a repository, so the crate manifest
    // directory is a root that must be refused. Using it rather than a
    // fabricated path means the witness fails if the detection stops working
    // against a real working tree.
    let inside = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(
        is_inside_repository(&inside),
        "the crate manifest directory is inside a repository; detection is broken"
    );

    match receipt_root(Some(inside.clone()), None, || None) {
        Err(CustodyRefusal::ReceiptRootInsideRepository { root }) => {
            assert_eq!(root, inside, "the refusal names the offending root");
        }
        other => panic!("an in-repository receipt root must be refused; got {other:?}"),
    }

    // A path several directories below the root is still inside it. Checking
    // only the directory itself is the version of this rule that passes review
    // and then fails in the field.
    let nested = inside.join("src").join("deeper");
    assert!(
        is_inside_repository(&nested),
        "detection must walk upward, not test one directory"
    );
    match receipt_root(Some(nested), None, || None) {
        Err(CustodyRefusal::ReceiptRootInsideRepository { .. }) => {}
        other => panic!("a nested in-repository root must be refused; got {other:?}"),
    }
}

#[test]
fn fixture_roots_are_distinct_and_removed_on_success_and_unwind() -> std::io::Result<()> {
    let first = outside_root();
    let first_path = first.to_path_buf();
    let second = outside_root();
    let second_path = second.to_path_buf();
    assert_ne!(first_path, second_path);
    std::fs::write(first.join("fixture"), b"owned fixture")?;
    drop(first);
    assert!(!first_path.exists());
    assert!(second_path.is_dir());
    let unwound = std::panic::catch_unwind(|| {
        let _owned = second;
        panic!("fixture panic exercises cleanup");
    });
    assert!(unwound.is_err());
    assert!(!second_path.exists());
    Ok(())
}

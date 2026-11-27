//! The R4H-L acceptance corpus is bound to the code that actually proves it.
//!
//! `fixtures/rust-reconstruction/r4-host-auth-foundation/` states four listener
//! laws language-neutrally. Prose alone releases nothing, so this owner proves
//! the corpus is a CLOSED typed document whose every scenario is owned by a real
//! test in a real compiled target, and whose action log states outcomes that
//! agree with the scenario claims.
//!
//! ## What R1 held this lane for, and what changed
//!
//! The first version of this owner was not fail-closed. It required only
//! nonempty strings for `contract` and friends, never pinning the admitted
//! contract or `schema_version`; it discarded the parsed `test_target`; it
//! reduced ownership to the substring `fn NAME(` in an arbitrary file; and it
//! read nothing of an action record beyond ordinal, scenario, actor and kind.
//! A review found three corruptions that passed against it: a fabricated
//! contract value, an owner pointing at an ordinary helper in an unrelated file,
//! and a fixed-bind scenario whose terminal bind expected `acquired: false`
//! while its claim required success.
//!
//! Each of those is now a refusal, and each has its own resurrection row in
//! `mutations/manifest.json` (QL10-QL15) so the refusal is shown to fail rather
//! than asserted.
//!
//! ## How the binding is structural
//!
//! `tests/support/mod.rs` is compiled into both this target and
//! `listener_contract`. This file checks the manifest against that inventory;
//! `listener_contract` proves the inventory's owners are real functions in
//! itself with a `[fn(); N]` array, so renaming one breaks compilation. The
//! target source is embedded here with `include_str!`, so it cannot vanish, and
//! owners are recognised by walking test attributes rather than by substring.
//!
//! Every guard fails closed: missing, unreadable, malformed, unknown-key,
//! duplicate, uncovered and contradictory are all failures, never a skip. That
//! matters more than usual here, because this corpus is not validated by the
//! The oracle's manifest gate — this is the only executable thing between it and
//! rot.
//!
//! The validators are pure functions over text. The negative tests drive them
//! with crafted inputs and never write a file, because writing into the
//! repository mid-battery would void the run measuring it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

mod support;

/// The declared target's source, embedded at compile time. If the file is
/// deleted or renamed, this target stops compiling.
const TARGET_SOURCE: &str = include_str!("listener_contract.rs");

include!("acceptance_corpus/manifest_owner_actions.rs");
include!("acceptance_corpus/typed_nested_semantics.rs");
include!("acceptance_corpus/fixtures_and_controls.rs");
include!("acceptance_corpus/authoritative_and_basic_negatives.rs");
include!("acceptance_corpus/placeholder_identity_and_hostile_negatives.rs");

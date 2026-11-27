//! Structural gate: consumers retain the store-owned transaction boundary.
//!
//! `tests/governed_writes.rs` proves that a write to a governed table is refused
//! WHEN IT GOES THROUGH the store. This scan refuses the removed mutable
//! accessor and consumer-owned transactions, and bounds read-accessor writes
//! to their reviewed owners and disposable fixtures.
//!
//! Behavioural and structural evidence answer different questions. A behavioural
//! test cannot see a call site nobody wrote a test for; a structural scan cannot
//! see what a call site does. Both are needed, and neither substitutes.
//!
//! # How the scan reads source
//!
//! A match is a violation unless it is on a line whose first non-space text is
//! `//`. That is deliberately stricter than stripping comment tails: a tail
//! stripper has to decide whether a `//` inside a string literal starts a
//! comment, and getting that wrong HIDES a real call site. Here a mention of a
//! forbidden name is legal only on a wholly-commented line, so the failure mode
//! is a false alarm on a trailing comment — reviewable — never a false pass.

use std::path::{Path, PathBuf};

/// Domains whose durable writers use the store-owned transaction boundary.
const CHOKEPOINT_OWNERS: &[&str] = &[
    "grapt-inbox",
    "grapt-form",
    "grapt-shell",
    "grapt-subagent",
    "grapt-permission",
    "grapt-instruction",
    "grapt-session",
    "grapt-service-state",
];

/// The raw accessor F3B1b removed. Nothing may reintroduce it.
const REMOVED_ACCESSOR: &str = "connection_mut";

/// Every way rusqlite hands out a transaction. Legal only inside `grapt-store`,
/// which owns the one that carries the denial.
const RAW_TRANSACTION_OPENERS: &[&str] = &[
    "transaction_with_behavior",
    "unchecked_transaction",
    ".transaction()",
];

const CONNECTION_OVERRIDES: &[&str] = &[
    "set_db_config",
    "create_scalar_function",
    "create_aggregate_function",
    "create_window_function",
    "remove_function",
    "create_collation",
    "remove_collation",
    "collation_needed",
];

const CHOKEPOINT_CALL: &str = "governed_transaction()";

/// Exact autocommit footprint through the read accessor. The inbox count includes
/// one R3 contention-fixture insert that establishes two independently claimed
/// durable operations before racing their governed parent admissions. A new
/// site, including a multiline method chain, changes this inventory and fails
/// review instead of evading the transaction count merely because it never
/// spells a rusqlite transaction opener.
// Retained campaign additions: form/inbox corruption witnesses (+2/+3),
// permission cancellation trigger install/remove (+2), shell schema migration
// and its legacy fixture (+2), product shell-binding schema and missing-table
// witness (+2). These add no alternate canonical-event write route.
//
// Form addition: grapt-form 4 -> 5, one further corruption
// witness in tests/form_service.rs. It writes four CONTRADICTORY rows --
// answered with no answer, cancelled while carrying one, pending yet
// suspended, and an answer that is not JSON -- and asserts `form_by_id`
// refuses each. Those rows exist precisely BECAUSE the governed path will not
// produce them, so the witness cannot be written through it; that is the
// property under test, not a way around the chokepoint. It adds no alternate
// canonical-event write route.
//
// The scan now also counts writes through a LOCAL
// BINDING of the accessor (`let conn = store.connection(); conn.execute*(..)`),
// which the chained form could not see. Re-pinned at base 03559fd7 by the same
// scan, every new hit being a `conn` binding: inbox +2 (tests/inbox_admission.rs),
// permission +4 (tests/permission_service.rs), session +5
// (tests/session_list_acceptance_corpus/execution.rs), shell +8 (src/service.rs
// 5: PSF's BEGIN IMMEDIATE, COMMIT, ROLLBACK and the CREATE/ALTER through the
// same `conn`; tests/shell_operation.rs 3), subagent +15
// (tests/subagent_operation/recovery_concurrency.rs). No other crate moved.
// SC-REL-04: each owner schema-atomicity fixture explicitly selects DELETE (+1
// form/inbox/shell). Product's commit-observer unit uses one checkpoint and
// its negative native witness uses two (+3); exact fixture bindings below keep
// these test-only PRAGMAs from admitting a production autocommit write.
const AUTOCOMMIT_SITES: &[(&str, usize)] = &[
    // Global-form tests: failure trigger install/remove and legacy schema seed.
    // FormService::initialize's autocommit CREATE is now
    // inside its one governed transaction (8 -> 7).
    // Presented-form rollback installs and removes one in-memory TEMP trigger.
    // Its exact test binding is checked with the inventory below.
    ("grapt-form", 10),
    // N02 adds one named in-memory instruction-table corruption witness.
    // The product fixture gate below pins its exact path and test body.
    // RW adds one in-memory projected-kind corruption witness.
    // Session-form restart recovery: one named disposable projection-corruption witness.
    // Two named reader-isolation fixtures hold and roll back the synthetic writer.
    ("grapt-product", 10),
    // R6 serving attestation: physical-byte perturbation and domain corruption.
    // The named witness below prevents moving these exceptions into production.
    // The shell owner keeps its tables available for stores an earlier build
    // wrote. Its CREATE batch and the legacy `time_started` ALTER run in one
    // explicit BEGIN IMMEDIATE on the read accessor, counted through the `conn`
    // binding; the DELETE witness is the schema-atomicity fixture.
    ("grapt-shell", 1),
    ("grapt-release", 2),
    // SCC sustained_history corrupts a selected message ID to prove refusal.
    // This is a test-only projection write, not a canonical-event write route.
    // Native-mail fixtures install the read seam and simulate binding revocation.
    // Retry-mode tests remove a promoted projection and damage a pending mode;
    // both in-memory corruption writes are bound to their exact fixture below.
    ("grapt-inbox", 18),
    // REL-15 fork_sequence_restart fixtures delete a projection to prove rebuild,
    // seed the exact legacy fork cache, and corrupt its through-sequence to prove
    // refusal. These three test-only projection writes add no production writer.
    ("grapt-instruction", 12),
    // One in-memory denial fixture drops approval storage before admission.
    ("grapt-permission", 7),
    // Fault fixtures use commit/progress hooks; only owned query-only and read-projection setup remains.
    ("grapt-runtime", 2),
    // Three session-list fixtures tamper a projection digest to prove bad rows
    // degrade instead of failing the page, the next page or the store open.
    // Test-only corrupt-row witnesses in tests/session_list_degraded.rs and tests/session_list_support/mod.rs.
    ("grapt-session", 21),
    // Followup schema creation stays in the existing schema owner; two
    // in-memory recovery writes remove only its additive tables.
    ("grapt-subagent", 26),
];

fn workspace_crates() -> PathBuf {
    // .../crates/grapt-store -> .../crates
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    match manifest.parent() {
        Some(parent) => parent.to_path_buf(),
        None => panic!("the crate must live under a workspace crates directory"),
    }
}

/// This gate's own source, which necessarily spells every forbidden name.
///
/// Excluded by EXACT path, not by file name, so a file called
/// `write_chokepoint.rs` anywhere else is still scanned. That the scan still
/// works after the exclusion is what `the_scan_catches_every_bypass_form`
/// proves, on synthetic sources this exclusion cannot reach.
fn this_gates_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("write_chokepoint.rs")
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let excluded = this_gates_source();
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => panic!("cannot read {}: {error}", root.display()),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => panic!("cannot read an entry under {}: {error}", root.display()),
        };
        let path = entry.path();
        if path.is_dir() {
            // `target` is build output, never source under review.
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            found.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") && path != excluded {
            found.push(path);
        }
    }
    found
}

/// Lines carrying `needle` in CODE, one-based. A wholly-commented line is prose.
fn code_hits(source: &str, needle: &str) -> Vec<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle) && !line.trim_start().starts_with("//"))
        .map(|(index, _)| index + 1)
        .collect()
}

fn autocommit_hits(source: &str) -> usize {
    let compact: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .flat_map(|line| line.chars().filter(|character| !character.is_whitespace()))
        .collect();
    compact.matches(".connection().execute(").count()
        + compact.matches(".connection().execute_batch(").count()
        + bound_connection_hits(&compact)
}

/// Writes through a LOCAL BINDING of the read accessor
/// (`let conn = store.connection(); conn.execute*(..)`), which the chained form
/// above cannot see. `compact` is the source with comments and whitespace
/// removed, so a binding reads `letconn=store.connection();` (or with `mut` or
/// a type annotation). Every `<name>.execute(` / `<name>.execute_batch(` in the
/// file counts, where `<name>` stands alone (not the tail of a longer name).
fn bound_connection_hits(compact: &str) -> usize {
    let mut names = std::collections::BTreeSet::new();
    for (end, _) in compact.match_indices(".connection();") {
        let statement_start = compact[..end]
            .rfind([';', '{', '}'])
            .map_or(0, |index| index + 1);
        let statement = &compact[statement_start..end];
        let Some(binding) = statement.strip_prefix("let") else {
            continue;
        };
        let binding = binding.strip_prefix("mut").unwrap_or(binding);
        let name: String = binding
            .chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .collect();
        if !name.is_empty() {
            names.insert(name);
        }
    }
    names
        .iter()
        .map(|name| {
            [".execute(", ".execute_batch("]
                .iter()
                .map(|call| {
                    let needle = format!("{name}{call}");
                    compact
                        .match_indices(&needle)
                        .filter(|(start, _)| {
                            compact[..*start].chars().next_back().is_none_or(|before| {
                                !(before.is_alphanumeric() || before == '_' || before == '.')
                            })
                        })
                        .count()
                })
                .sum::<usize>()
        })
        .sum()
}

fn crate_of(path: &Path, crates: &Path) -> String {
    match path.strip_prefix(crates).ok().and_then(|rest| {
        rest.components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
    }) {
        Some(name) => name,
        None => panic!("{} is not inside the crates directory", path.display()),
    }
}

fn read(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => normalized_source(&text),
        Err(error) => panic!("cannot read {}: {error}", path.display()),
    }
}

fn normalized_source(text: &str) -> String {
    // Git's native Windows checkout changes line endings, not source structure.
    text.replace("\r\n", "\n")
}

#[test]
fn structural_markers_survive_native_checkout_line_endings() {
    let lf = "#[cfg(test)]\n#[path = \"fixture.rs\"]\nmod tests;\n";
    assert_eq!(normalized_source(&lf.replace('\n', "\r\n")), lf);
    assert_eq!(normalized_source(lf), lf);
}

fn named_test_body<'a>(source: &'a str, marker: &str) -> &'a str {
    let after = match source.split_once(marker) {
        Some((_, after)) => after,
        None => panic!("named structural witness {marker:?} is absent"),
    };
    match after.split_once("\n#[test]") {
        Some((body, _)) => body,
        None => after,
    }
}

#[test]
fn the_removed_raw_accessor_has_zero_survivors() {
    let crates = workspace_crates();
    let mut survivors = Vec::new();
    for path in rust_sources(&crates) {
        for line in code_hits(&read(&path), REMOVED_ACCESSOR) {
            survivors.push(format!("{}:{line}", path.display()));
        }
    }
    assert!(
        survivors.is_empty(),
        "`{REMOVED_ACCESSOR}` was removed by F3B1b and must not return: {survivors:?}"
    );
}

#[test]
fn only_the_store_may_open_a_transaction() {
    let crates = workspace_crates();
    let mut escapes = Vec::new();
    for path in rust_sources(&crates) {
        let owner = crate_of(&path, &crates);
        if owner == "grapt-store" {
            continue;
        }
        let source = read(&path);
        for opener in RAW_TRANSACTION_OPENERS.iter().chain(CONNECTION_OVERRIDES) {
            let hits = code_hits(&source, opener);
            if *opener == "create_scalar_function"
                && path == crates.join("grapt-session/src/list/query.rs")
            {
                assert_eq!(hits.len(), 1, "only the fixed directory function is exempt");
                continue;
            }
            for line in hits {
                escapes.push(format!("{}:{line} ({opener})", path.display()));
            }
        }
    }
    assert!(
        escapes.is_empty(),
        "connection authority outside grapt-store can bypass the governed-write denial: {escapes:?}"
    );
}

#[test]
fn each_durable_domain_uses_the_chokepoint() {
    let crates = workspace_crates();
    for name in CHOKEPOINT_OWNERS {
        let root = crates.join(name);
        let found: usize = rust_sources(&root.join("src"))
            .iter()
            .filter(|path| {
                !path.components().any(|part| part.as_os_str() == "tests")
                    && !path.file_stem().is_some_and(|name| {
                        name == "tests" || name.to_string_lossy().ends_with("_tests")
                    })
            })
            .map(|path| {
                code_hits(
                    read(path).split("#[cfg(test)]").next().unwrap_or_default(),
                    CHOKEPOINT_CALL,
                )
                .len()
            })
            .sum();
        assert!(
            found > 0,
            "{name} no longer uses the governed transaction boundary"
        );
    }
}

#[test]
fn admission_mode_lookup_and_corruption_writes_are_exactly_bound() {
    let root = workspace_crates().join("grapt-inbox");
    let owner = read(&root.join("src/admission/existing.rs"));
    let lookup = named_test_body(&owner, "pub fn admitted_delivery(");
    assert_eq!(code_hits(lookup, CHOKEPOINT_CALL).len(), 1);
    assert_eq!(autocommit_hits(lookup), 0);
    assert!(!lookup.contains("append"));
    assert!(!lookup.contains(".execute"));
    let fixture = read(&root.join("tests/inbox_admission/delivery_lookup.rs"));
    assert_eq!(autocommit_hits(&fixture), 2);
    for name in [
        "fn stored_modes_survive_promotion_and_lookup_is_read_only()",
        "fn damaged_delivery_is_an_error_rather_than_an_absent_admission()",
    ] {
        let witness = named_test_body(&fixture, name);
        assert_eq!(autocommit_hits(witness), 1);
        assert!(witness.contains("fresh()?"));
        assert!(witness.contains("Err(InboxError::Corruption { .. })"));
    }
}

#[test]
fn session_config_binding_inventory_is_bound_to_its_fenced_audit_append() {
    let source = read(&workspace_crates().join("grapt-session/src/config_binding.rs"));
    assert_eq!(code_hits(&source, CHOKEPOINT_CALL).len(), 1);
    let binding = named_test_body(&source, "pub fn bind(");
    assert!(binding.contains(".fencing_token(&aggregate)"));
    assert!(binding.contains("crate::SessionStore::new(&tx)"));
    assert!(binding.contains("EventStore::append_within("));
    assert!(binding.contains("kind_name: \"session.config.bound\""));
    assert!(binding.contains("tx.commit()"));
    assert!(!binding.contains("elect_writer("));
}

#[test]
fn lifecycle_hook_inventory_is_bound_to_advisory_owner_and_governed_wire_fixture() {
    let root = workspace_crates().join("grapt-session");
    let owner = read(&root.join("src/notice/lifecycle_hook.rs"));
    assert_eq!(code_hits(&owner, CHOKEPOINT_CALL).len(), 1);
    let record = named_test_body(&owner, "fn record(");
    assert!(record.contains("store.fencing_token(&aggregate)"));
    assert!(record.contains("append_in(&tx, aggregate, writer, authorities, &value)?"));
    assert!(!record.contains("elect_writer("));
    let fixture = read(&root.join("tests/lifecycle_hook_notices.rs"));
    assert_eq!(code_hits(&fixture, CHOKEPOINT_CALL).len(), 1);
    let append = named_test_body(&fixture, "fn append_wire(");
    assert!(append.contains("store.fencing_token(&aggregate)?"));
    assert!(append.contains("EventStore::append_within("));
    assert!(fixture.contains("EventStore::open_in_memory()?"));
    assert_eq!(autocommit_hits(&owner), 0);
    assert_eq!(autocommit_hits(&fixture), 0);
}

#[test]
fn r3_delivery_inventory_is_bound_to_its_two_named_inbox_witnesses() {
    let source = read(
        &workspace_crates()
            .join("grapt-inbox")
            .join("tests")
            .join("inbox_admission.rs"),
    );
    let contention = named_test_body(
        &source,
        "fn concurrent_operation_deliveries_share_one_atomic_last_slot()",
    );
    let no_forge = named_test_body(
        &source,
        "fn arbitrary_json_has_no_public_transactional_admission_escape()",
    );

    assert_eq!(
        code_hits(contention, CHOKEPOINT_CALL).len(),
        1,
        "the last-slot contention witness owns one governed transaction"
    );
    assert_eq!(
        code_hits(no_forge, CHOKEPOINT_CALL).len(),
        1,
        "the forged-authority refusal witness owns one governed transaction"
    );
    assert_eq!(
        autocommit_hits(contention),
        1,
        "the contention fixture owns exactly one reviewed autocommit setup site"
    );
    assert_eq!(
        autocommit_hits(no_forge),
        0,
        "the forged-authority witness must not add an autocommit write"
    );
}

#[test]
fn native_mail_inventory_is_bound_to_admission_hold_and_in_memory_fixtures() {
    let crates = workspace_crates();
    let owner = read(&crates.join("grapt-inbox/src/admission/native_mail.rs"));
    assert_eq!(code_hits(&owner, CHOKEPOINT_CALL).len(), 2);
    let admission = match owner.split_once("pub fn admit_native_mail_wake(") {
        Some((_, tail)) => match tail.split_once("pub fn pending_native_mail_wake(") {
            Some((body, _)) => body,
            None => panic!("native wake recovery boundary is absent"),
        },
        None => panic!("native wake admission is absent"),
    };
    let hold = match owner.split_once("pub fn hold_native_mail_wakes(") {
        Some((_, tail)) => tail,
        None => panic!("native wake hold is absent"),
    };
    assert_eq!(code_hits(admission, CHOKEPOINT_CALL).len(), 1);
    assert_eq!(code_hits(hold, CHOKEPOINT_CALL).len(), 1);
    assert_eq!(autocommit_hits(&owner), 0);
    let state = read(&crates.join("grapt-inbox/src/admission/native_mail_state.rs"));
    assert_eq!(code_hits(&state, CHOKEPOINT_CALL).len(), 3);
    let resume = match state
        .split_once("pub fn resume_native_mail_target(")
        .and_then(|(_, tail)| tail.split_once("pub fn native_notice_target("))
    {
        Some((body, _)) => body,
        None => panic!("explicit native resume boundary is absent"),
    };
    assert_eq!(code_hits(resume, CHOKEPOINT_CALL).len(), 1);
    assert_eq!(autocommit_hits(&state), 0);
    let state_fixture = read(&crates.join("grapt-inbox/tests/native_mail_wake/state.rs"));
    assert_eq!(code_hits(&state_fixture, CHOKEPOINT_CALL).len(), 1);
    assert_eq!(autocommit_hits(&state_fixture), 0);
    let fixture = read(&crates.join("grapt-inbox/tests/native_mail_wake.rs"));
    assert!(fixture.contains("initialize(EventStore::open_in_memory()?)"));
    assert_eq!(
        autocommit_hits(named_test_body(&fixture, "fn initialize(")),
        1
    );
    let rebound = named_test_body(
        &fixture,
        "fn seat03_rebinding_before_commit_refuses_without_consuming_work()",
    );
    assert_eq!(autocommit_hits(rebound), 1);
    assert!(rebound.contains("UPDATE instruction_entry SET value='rebound'"));
    assert_eq!(autocommit_hits(&fixture), 2);
}

#[test]
fn subagent_inventory_is_bound_to_seven_transition_paths_and_exact_fixture_files() {
    let crate_root = workspace_crates().join("grapt-subagent");
    let service = read(&crate_root.join("src").join("service.rs"));
    let delivery = read(&crate_root.join("src/service/delivery.rs"));
    for transition in [
        "pub fn claim_on(",
        "pub fn deliver_on(",
        "pub fn acknowledge_on(",
    ] {
        assert!(
            delivery.contains(transition),
            "named delivery transition {transition:?} is absent"
        );
    }
    assert_eq!(
        code_hits(&delivery, CHOKEPOINT_CALL).len(),
        3,
        "the delivery owner must retain its three governed transitions"
    );
    for transition in [
        "pub fn cleanup(",
        "pub fn reopen_if_undelivered(",
        "pub fn settle_dead_generation(",
        "fn cas_status(",
    ] {
        assert!(
            service.contains(transition),
            "named subagent transition {transition:?} is absent"
        );
    }
    assert_eq!(
        code_hits(&service, CHOKEPOINT_CALL).len(),
        4,
        "the remaining four named subagent transitions must retain their inventory"
    );
    let mail = read(&crate_root.join("tests/subagent_operation/mail_presentation.rs"));
    assert_eq!(
        code_hits(&mail, CHOKEPOINT_CALL).len(),
        2,
        "result-mail fixtures own only reservation/capture and generic-append refusal transactions"
    );

    let expected_autocommit = [
        ("src/service.rs", 3),
        ("tests/subagent_operation/delivery_persistence.rs", 3),
        ("tests/subagent_operation/durable_delivery_r3.rs", 2),
        // 1 chained + 15 writes through a local `conn`
        // binding of the accessor, which the scan now counts.
        ("tests/subagent_operation/recovery_concurrency.rs", 16),
        (
            "tests/subagent_operation/mail_presentation/agent_messages/followups.rs",
            2,
        ),
    ];
    for (relative, expected) in expected_autocommit {
        assert_eq!(
            autocommit_hits(&read(&crate_root.join(relative))),
            expected,
            "subagent autocommit inventory drifted in {relative}"
        );
    }
}

#[test]
fn release_autocommit_inventory_belongs_only_to_serving_attestation_fixture() {
    let root = workspace_crates().join("grapt-release");
    let fixture = root.join("tests/r6_migration.rs");
    let source = read(&fixture);
    let witness = named_test_body(
        &source,
        "fn serving_attestation_measures_the_open_store_and_rejects_changed_domain()",
    );
    assert_eq!(
        autocommit_hits(witness),
        2,
        "serving attestation owns two fixture writes"
    );
    for path in rust_sources(&root) {
        assert_eq!(
            autocommit_hits(&read(&path)),
            if path == fixture { 2 } else { 0 },
            "release fixture exception escaped into {}",
            path.display()
        );
    }
}

#[test]
fn permission_denial_fixture_owns_its_one_in_memory_schema_write() {
    let source =
        read(&workspace_crates().join("grapt-permission/src/service/http/consequences.rs"));
    let witness = named_test_body(
        &source,
        "fn one_denied_consequence_refuses_the_whole_tool_before_approval_reads()",
    );
    let witness = witness
        .split_once("\n    #[test]")
        .map_or(witness, |(body, _)| body);
    assert_eq!(autocommit_hits(&source), 1);
    assert_eq!(autocommit_hits(witness), 1);
    assert!(witness.contains("EventStore::open_in_memory()?"));
    assert!(witness.contains("DROP TABLE permission_http_rule"));
}

#[test]
fn product_autocommit_addition_is_bound_to_unreadable_native_identity_fixture() {
    let root = workspace_crates().join("grapt-product");
    let fixture = root.join("src/dogfood/native_mail/tests.rs");
    let source = read(&fixture);
    let witness = named_test_body(
        &source,
        "fn unreadable_instruction_owner_is_not_an_absent_binding()",
    );
    assert_eq!(autocommit_hits(witness), 1);
    assert!(witness.contains("let context = context(None, false)?"));
    assert!(witness.contains("DROP TABLE instruction_entry"));
    assert!(witness.contains("context.current_binding(), Err(E::Binding)"));
    let seed = named_test_body(&source, "fn context(");
    assert!(seed.contains("EventStore::open_in_memory()?"));
    let existing = root.join("src/dogfood/shells.rs");
    let rewind = root.join("src/dogfood/rewind_selection_tests.rs");
    let rewind_source = read(&rewind);
    let witness = named_test_body(
        &rewind_source,
        "fn rw_pending_input_and_unknown_projection_refuse_without_a_cut(",
    );
    assert_eq!(autocommit_hits(witness), 1);
    assert!(witness.contains("UPDATE session_message SET type = 'future-user'"));
    assert!(rewind_source.contains("EventStore::open_in_memory()?"));
    let observer = root.join("src/dogfood/startup_fixture.rs");
    let observer_source = read(&observer);
    let observer_witness = named_test_body(
        &observer_source,
        "fn a_checkpointed_wal_write_remains_visible_to_the_startup_commit_observer(",
    );
    assert_eq!(autocommit_hits(observer_witness), 1);
    assert!(observer_source.contains("#[cfg(test)]\nmod tests"));
    assert!(observer_witness.contains("PRAGMA wal_checkpoint(TRUNCATE)"));
    assert!(observer_witness.contains("install("));
    assert!(observer_witness.contains("assert_eq!(observed, b\"C\")"));
    let checkpoint = root.join("tests/dogfood_headless/startup_sync.rs");
    let checkpoint_source = read(&checkpoint);
    let checkpoint_witness = named_test_body(
        &checkpoint_source,
        "fn a_checkpointed_write_is_rejected_by_the_zero_startup_commit_assertion()",
    );
    assert_eq!(autocommit_hits(checkpoint_witness), 2);
    assert!(checkpoint_witness.contains("PRAGMA wal_checkpoint(TRUNCATE)"));
    assert!(checkpoint_witness.contains("assert_commits(&root, \"restart\", 1)"));
    assert!(checkpoint_witness.contains("catch_unwind(|| assert_commits(&root, \"restart\", 0))"));
    let session_form = root.join("src/dogfood/form_tests.rs");
    let readers = root.join("src/dogfood/continuation/tests/history_read_isolation.rs");
    for path in rust_sources(&root) {
        let expected =
            if path == fixture || path == rewind || path == observer || path == session_form {
                1
            } else if path == existing || path == checkpoint {
                2
            } else if path == readers {
                4
            } else {
                0
            };
        assert_eq!(
            autocommit_hits(&read(&path)),
            expected,
            "product fixture exception escaped into {}",
            path.display()
        );
    }
}

#[test]
fn history_reader_transactions_belong_only_to_disposable_writer_hold_fixtures() {
    let root = workspace_crates().join("grapt-product/src/dogfood");
    let owner = normalized_source(&read(&root.join("continuation.rs")));
    assert!(owner.contains("#[cfg(test)]\npub(in crate::dogfood) mod tests;"));
    let parent = normalized_source(&read(&root.join("continuation/tests/history_page.rs")));
    assert!(parent.contains("#[path = \"history_read_isolation.rs\"]\nmod read_isolation;"));
    let source = read(&root.join("continuation/tests/history_read_isolation.rs"));
    let source = normalized_source(&source);
    assert_eq!(autocommit_hits(&source), 4);
    for (marker, boundary, fixture) in [
        (
            "async fn independent_read(",
            "\n#[tokio::test",
            "long_session(",
        ),
        (
            "fn provider_confirmed_usage_does_not_wait_for_writer_ownership()",
            "\n#[test]",
            "Fixture::new(",
        ),
    ] {
        let witness = named_test_body(&source, marker);
        let witness = witness
            .split_once(boundary)
            .map_or(witness, |(body, _)| body);
        assert_eq!(autocommit_hits(witness), 2, "{marker}");
        assert!(witness.contains(fixture), "{marker}");
        assert_eq!(
            witness
                .matches(".execute_batch(\"BEGIN IMMEDIATE\")")
                .count(),
            1
        );
        assert_eq!(witness.matches(".execute_batch(\"ROLLBACK\")").count(), 1);
    }
}

#[test]
fn session_form_corruption_write_belongs_only_to_disposable_recovery_fixture() {
    let root = workspace_crates().join("grapt-product/src/dogfood");
    let source = read(&root.join("form_tests.rs"));
    let witness = named_test_body(
        &source,
        "fn corrupt_pending_form_is_refused_without_acquiring_a_writer()",
    );
    assert_eq!(autocommit_hits(witness), 1);
    assert!(read(&root.join("form.rs")).contains("#[cfg(test)]\n#[path = \"form_tests.rs\"]"));
    assert!(witness.contains("let root = Root::new()?"));
    assert!(witness.contains("UPDATE form_request SET title='corrupt projection' WHERE id=?1"));
    assert!(witness.contains("Err(FormReplyApplicationError::InvalidRequest(_))"));
    assert!(witness.contains("Err(FormCancelApplicationError::InvalidRequest(_))"));
    assert!(witness.contains("epoch(&app, &owner)?"));
    assert!(witness.contains("corruption must not elect a writer"));
    assert!(witness.contains("facts(&app, &owner)?, [\"form.created.1\"]"));
}

#[test]
fn rollback_journal_overrides_belong_only_to_schema_atomicity_fixtures() {
    for owner in ["form", "inbox", "shell"] {
        let source =
            read(&workspace_crates().join(format!("grapt-{owner}/tests/schema_single_commit.rs")));
        let witness = named_test_body(
            &source,
            &format!("fn the_{owner}_schema_is_created_in_one_commit_and_kept_in_none()"),
        );
        assert_eq!(autocommit_hits(&source), 1, "{owner}");
        assert_eq!(autocommit_hits(witness), 1, "{owner}");
        assert!(witness.contains("PRAGMA journal_mode = DELETE"), "{owner}");
        assert!(
            witness.contains("EventStore::open(&database, \"0.1.0\")?"),
            "{owner}"
        );
        assert!(witness.contains("fresh + 1"), "{owner}");
        assert!(witness.contains("a current"), "{owner}");
    }
}

#[test]
fn runtime_autocommit_is_limited_to_read_only_and_projection_fixtures() {
    let root = workspace_crates().join("grapt-runtime");
    let existing = root.join("src/attempt_admission/tests/session_application/runtime_and_harness/steer_interrupt_and_store_failures.rs");
    let wake = root.join("src/mailbox/remote/tests/v2/wake.rs");
    for path in rust_sources(&root) {
        let expected = usize::from(path == existing || path == wake);
        assert_eq!(
            autocommit_hits(&read(&path)),
            expected,
            "runtime fixture exception escaped into {}",
            path.display()
        );
    }
}

/// Every crate whose read-accessor autocommit count differs from `inventory`,
/// ALL of them, so one failure names every drift rather than only the first.
fn inventory_drift(crates: &Path, inventory: &[(&str, usize)]) -> Vec<String> {
    let mut drift = Vec::new();
    for (name, expected) in inventory {
        let found: usize = rust_sources(&crates.join(name))
            .iter()
            .map(|path| autocommit_hits(&read(path)))
            .sum();
        if found != *expected {
            drift.push(format!("{name}: expected {expected}, found {found}"));
        }
    }
    drift
}

#[test]
fn autocommit_writes_through_the_read_accessor_are_exactly_inventoried() {
    let crates = workspace_crates();
    let form = crates.join("grapt-form");
    let fixture = read(&form.join("tests/form_service.rs"));
    let rollback = named_test_body(
        &fixture,
        "fn presentation_metadata_is_bounded_and_creation_rolls_back_with_its_projection()",
    );
    assert_eq!(autocommit_hits(rollback), 2);
    assert!(rollback.contains("EventStore::open_in_memory()?"));
    assert!(rollback.contains("CREATE TEMP TRIGGER refuse_presented"));
    assert!(rollback.contains("DROP TRIGGER refuse_presented"));
    assert_eq!(autocommit_hits(&read(&form.join("src/service.rs"))), 0);
    let drift = inventory_drift(&crates, AUTOCOMMIT_SITES);
    assert!(
        drift.is_empty(),
        "autocommit read-accessor inventory drifted in {} crate(s):\n  {}",
        drift.len(),
        drift.join("\n  ")
    );
    let inventoried: Vec<&str> = AUTOCOMMIT_SITES.iter().map(|(name, _)| *name).collect();
    for source in rust_sources(&crates) {
        let owner = crate_of(&source, &crates);
        if owner == "grapt-store" || inventoried.contains(&owner.as_str()) {
            continue;
        }
        assert_eq!(
            autocommit_hits(&read(&source)),
            0,
            "{} opens an unreviewed autocommit write through the read accessor",
            source.display()
        );
    }
}

#[test]
fn the_autocommit_scan_catches_single_and_multiline_chains() {
    let shapes = [
        "store.connection().execute(\"DELETE FROM x\", [])",
        "store.connection()\n    .execute_batch(\"DROP TABLE x\")",
        // Writes through a local binding of the accessor.
        "let conn = store.connection();\nconn.execute_batch(\"DROP TABLE x\")?;",
        "let mut c: &rusqlite::Connection = self.store.connection();\nc.execute(\"DELETE FROM x\", [])?;",
    ];
    for shape in shapes {
        assert_eq!(
            autocommit_hits(shape),
            1,
            "HYG103 autocommit scan missed {shape:?}"
        );
    }
    // Not a write through the accessor: a connection opened some other way, a
    // longer name that merely ends in the bound one, and a method on a field.
    let clean = [
        "let other = rusqlite::Connection::open(path)?;\nother.execute(\"X\", [])?;",
        "let conn = store.connection();\nmyconn.execute(\"X\", [])?;",
        "let conn = store.connection();\nself.conn.execute(\"X\", [])?;",
    ];
    for shape in clean {
        assert_eq!(
            autocommit_hits(shape),
            0,
            "autocommit scan over-counted {shape:?}"
        );
    }
}

/// The inventory check reports EVERY drifted crate in one failure.
#[test]
fn the_inventory_check_reports_every_drift_at_once() {
    let crates = workspace_crates();
    let wrong: Vec<(&str, usize)> = AUTOCOMMIT_SITES
        .iter()
        .take(2)
        .map(|(name, expected)| (*name, expected + 1))
        .collect();
    let drift = inventory_drift(&crates, &wrong);
    assert_eq!(
        drift.len(),
        2,
        "HYG104 the inventory check stopped before naming every drift: {drift:?}"
    );
}

#[test]
fn the_scan_catches_every_bypass_form() {
    // A gate that misses a shape is worth nothing, because drift takes the shape
    // the gate misses. Each case is source the scan MUST flag.
    let flagged = [
        ("plain call", "    let c = store.connection_mut();"),
        (
            "method chain on one line",
            "    let tx = self.store.connection_mut().transaction_with_behavior(b)?;",
        ),
        (
            "trailing comment after real code",
            "    let c = store.connection_mut(); // still a call",
        ),
        (
            "indented inside a block",
            "            store.connection_mut().execute(sql, [])?;",
        ),
    ];
    for (shape, source) in flagged {
        assert!(
            !code_hits(source, REMOVED_ACCESSOR).is_empty(),
            "the {shape} route hid a raw connection from the scan"
        );
    }

    // And source it must NOT flag, or the gate is noise nobody can satisfy: this
    // file and the store's own module docs both name the accessor in prose.
    let prose = [
        "    /// It replaces `connection_mut`, which returned a raw connection.",
        "    // connection_mut is gone; use governed_transaction.",
        "//! Before this module the protection was a convention: `connection_mut`",
    ];
    for line in prose {
        assert!(
            code_hits(line, REMOVED_ACCESSOR).is_empty(),
            "a wholly-commented mention is prose, not a call site: {line:?}"
        );
    }
}

#[test]
fn the_scan_reaches_the_crates_it_claims_to() {
    // Without this, a walk that silently returned nothing would report every
    // crate as clean. Check the walk independently of its writer inventory.
    let crates = workspace_crates();
    let sources = rust_sources(&crates);
    assert!(
        sources.len() > 50,
        "the walk found only {} Rust sources under {}",
        sources.len(),
        crates.display()
    );
    for name in CHOKEPOINT_OWNERS {
        assert!(
            sources.iter().any(|path| crate_of(path, &crates) == *name),
            "the walk never reached {name}"
        );
    }
}

/// Store-owned fixture injection is a named, counted writer: it exists only
/// to stage the counterexample rows no admission would accept, and the files
/// that may hold it are pinned exactly, the same way the autocommit
/// inventory pins its sites.
#[test]
fn fixture_rows_are_bound_to_named_counterexample_fixtures() {
    let crates = workspace_crates();
    let root = crates.join("grapt-product/src/dogfood/continuation/tests");
    let expected = [
        ("recent.rs", 4),
        ("recent_bounds.rs", 2),
        ("recent_overflow.rs", 3),
        ("recent_window.rs", 2),
    ];
    for (name, count) in expected {
        let path = root.join(name);
        assert_eq!(
            code_hits(&read(&path), "fixture_rows()").len(),
            count,
            "fixture-row injection drifted in {name}"
        );
    }
    let restart = crates.join("grapt-inbox/tests/native_mail_wake.rs");
    assert_eq!(code_hits(&read(&restart), "fixture_rows()").len(), 1);
    for path in rust_sources(&crates) {
        if path.starts_with(&root) || path == restart {
            continue;
        }
        assert!(
            code_hits(&read(&path), "fixture_rows()").is_empty(),
            "fixture-row injection escaped the named fixtures: {}",
            path.display()
        );
    }
}

//! Compile-time ownership probes (foundational-types contract §"Compile-time
//! ownership probes required before R1"). The compiler must REJECT each
//! probe file; a probe that starts compiling is a broken law.
//!
//! Numbering follows the eighteen rows of that section. Coverage:
//!
//! | Row | Probe | Note |
//! | --- | --- | --- |
//! | 1  | `01_wrong_id_kind` | Permission, Form, Operation and ServiceInstance targets |
//! | 2  | `02_persist_session_grant`, `02b_session_grant_not_persistable` | sealed minting; no `Clone`, no `ToString` |
//! | 3  | `03_suspended_without_reason`, `03b_cross_family_blocker`, `03c_foreign_blocker_family` | missing reason, reply-as-reason and reason-as-reply; the independently-mixable two-parameter shape is gone; and the family trait is sealed |
//! | 4  | — | provider-owned; lives in `grapt-provider/tests/probes` so each request family fails on its own unsupported fields |
//! | 5  | `05_nonexhaustive_kind_match`, `05b_nonexhaustive_operation_terminal` | OperationKind and OperationTerminal |
//! | 6  | `06_double_settle` | |
//! | 7  | `07_raw_string_identity` | session, operation, effective-graph and service |
//! | 8  | `08_authored_capability`, `08b_lease_consumed_once`, `19_lease_requires_a_grant`, `30_settlement_mints_session_authority`, `31_authored_capability_grant`, `32_mint_a_live_decision`, `32b_assemble_a_live_decision` | split by error phase; `19` proves a lease cannot be minted without a grant that covers it; `30`-`32b` are the predecessor-chain exploits. `08b` takes its lease as a FUNCTION ARGUMENT so nothing can fail before the move: built in-probe, its only error was the removed constructor, which fired ahead of both `consume()` calls and duplicated `19` instead of witnessing linear non-reuse |
//! | 9  | `09_secret_material` | Display, Hash and equality all absent |
//! | 10 | `10_tool_schema_identity` | **partial** — see below |
//! | 11 | `11_ambient_authority` + the Clippy gate | see below |
//! | 12 | `12_checkpoint_ref_confusion`, `41_restore_without_a_lease`, `41b_apply_one_plan_twice`, `42_substitute_a_restore_plan`, `43_settle_a_restore_plan`, `43b_forge_a_restore_rescue`, `43c_author_a_settled_restore_plan`, `44_observe_restore_preconditions` | BOTH clauses. `12` witnesses the identity confusions — commit, artifact, compaction, Session fork, restore-plan id and a movable head — now against the REAL boundary rather than probe-local stubs. The second clause, "applying a restore without the exact settled plan/authority lease", had no subject at all until this round: there was no restore API in the crate, so the old probe declared its own `restore()` and `apply_plan()` and proved nothing about either. `41`-`44` cover omission of the lease, re-application of one settlement, plan substitution, and the seals on the plan, its rescue and its precondition observation. `44` is the one that closes "preconditions stored but never observed": the plan bound provider version, negotiated lanes and worktree quiescence, and nothing read them until `apply` began demanding a plan-bound observation whose mint this crate cannot reach |
//! | 13-18, §3 | — | retired with the campaign draft feature; no subject remains |
//! | §10 | `23_authored_model_family`, `24_forge_catalog_entry` | base model and family are catalog truth: no public constructor takes them, and only a loaded `ModelCatalog` issues the entry a `ModelRef` is built from |
//! | §7.5 | `25_authored_user_selection`, `26_foreign_selection_authority` | a user model selection cannot be authored as data, and no foreign type may declare itself the selection authority |
//! | `CORRECT` (provider tool) | `27_receipt_string_as_close_proof`, `28_forge_close_proof`, `29_rescope_tool_handle` | a parseable receipt is audit identity, not proof; the close proof is minted by the adapter lifecycle; and a handle cannot be re-scoped away from the proof that minted it |
//! | §7.5 root | `38_forge_user_selection_chain` | the COMPLETE predecessor chain: sealing a trait stops a foreign type implementing it and does nothing about calling the public methods of the impl that ships, so `UserSelectionSurface::establish` is now crate-private |
//! | provider root | `39_forge_provider_tool_chain` | the complete forge chain from `enter` to a durable handle; every link consumed its predecessor, but the first link rested on no adapter fact |
//! | §10 root | `40_rogue_catalog_family_claim`, `40b_rogue_catalog_derives_capability` | an attacker authors BOTH rows of its own snapshot, so same-snapshot identity is not authority; the catalog stays public as inert coordinates while the two authority-producing operations — the `agent default` boundary and capability derivation — are sealed |
//! | §10 root (metadata) | `45_manual_family_comparison`, `45b_catalog_enumerates_families`, `45c_entry_yields_its_row` | sealing the `same_family` PREDICATE was not enough: the coordinates it compares were readable, so `parent.provider()==child.provider() && parent.family()==child.family()` rebuilt the decision by hand — and rebuilt it WORSE, because the hand-rolled version drops the same-snapshot clause and answers `true` across two catalogs where the real predicate answers `false`. There is now no public read path to `family`/`base_model` on a `ModelRef`, a `ModelCatalogRecord`, a `ModelCatalogEntry`, a `ResolvedModelTarget`, or through `ModelCatalog::records`. `45b` witnesses a DOUBLED seal — unsealing `records` alone leaves the row accessor firing |
//! | §4.2 | `46_compaction_without_next_request` | the Step/request-context reference that reassembles the next request is a REQUIRED coordinate of `CompactionDescriptorParts`, and requiredness is the half of that law no mutation can witness: every edit that makes the coordinate omissible stops the crate compiling, and the mutation harness correctly refuses a mutant that does not compile. The other ten coordinates are `todo!()` so `!` coerces everywhere and E0063 is the sole error phase |
//! | §10 root (read channels) | `47_target_debug_prints_authority`, `47b_target_clone_reuses_authority`, `47c_target_equality_oracle`, `47d_target_capability_is_plaintext`, `48_catalog_row_equality_oracle`, `48b_catalog_row_ordering_and_hash_oracle` | sealing the ACCESSORS left three other read channels open, which is the same lesson one level up. Derived `Debug` printed the sealed base model and family verbatim. Derived `PartialEq`/`Hash` over the whole row was an enumeration ORACLE — author a row guessing a family, compare against a genuine ref, and equality returns one bit per guess. And `ResolvedModelTarget::capability()` was worse than an oracle: a `ModelTargetCapabilityId`'s canonical form literally contains `base=`/`family=`, so it was a PLAINTEXT DUMP. `Clone` on the target and the snapshot made a proof of authority reusable where the master requires one attempt, one effect. `48b` witnesses a DOUBLED seal: `ModelRef`'s narrowed equality cannot be reverted alone, because the row itself no longer has `PartialEq`/`Hash`. `47d` is the row L1 re-cut: it stays E0624 (privacy) rather than drifting to E0432 only because `ModelTargetCapabilityId` is re-exported at the crate root, so the probe still witnesses the seal instead of an unresolved import |
//! | §8.1 (capability identity) | `54_capability_namespace_union` | a model TARGET capability and an EFFECT capability are different identities. Both were spelled `CapabilityId`, and the merge was load-bearing rather than untidy: `CapabilityId` derives `PartialEq` and equality IS the authorization comparison, so two canonical forms that could ever coincide would compare equal ACROSS the boundary and a routing identity would answer for an effect grant. The nominal split makes all three substitutions unwritable — a route where an effect is required, an effect where a route is required, and the one that mattered, `CapabilityGrant::authorizes` answered with a routing id. The value is taken as a FUNCTION ARGUMENT because `ModelTargetCapabilityId` has no producer outside the crate, and building one would fail on the mint in an earlier phase and hide all three. One error phase, three E0308s. The RUNTIME half — that the three canonical namespaces (`model-target|`, the five `EffectClass` literals, and `restore-plan|`) stay pairwise disjoint — is `capability::tests::the_three_canonical_namespaces_are_pairwise_disjoint`, because disjointness of the encodings is a property of bytes rather than of types |
//! | R3a (seat address) | `50_seat_recipient_is_not_a_session` | a `SeatRecipientId` is an ADDRESS, and the three substitutions that would quietly turn it into something else all fail to compile: a `SessionId` (a conversation ends; an address does not), an `AgentId` (two seats may run one actor, and mail is delivered to the seat), and the reverse — a recipient standing in for a `MailboxMessageId`. One error phase, three E0308s |
//! | §4.3 (durability) | `49_reconstituted_issuance_authority` | a `SummaryCompatibility` rebuilt from persisted coordinates is inert DATA: it cannot answer `is_same_issuance`, cannot yield a `ModelRef`, and cannot convert into one. This is the STRUCTURAL half of the restart adversary, and no runtime test can witness it, because the code that would witness it does not compile — the same reason `46` is a probe. The behavioural half is executed in `grapt-event/tests/summary_restart_adversary.rs` |
//! | R4D-2V (service version) | `53_registration_version_byte_order` | a `ServiceRegistrationVersion` has no byte order. `opaque_id!` derives `PartialOrd`/`Ord`, and for THIS type that derivation is a lexicographic order over the recorded bytes, under which `"0.10.0"` sorts BELOW `"0.9.0"` — so a caller reaching for `>` to ask "is the candidate newer" compiles and answers backwards, making an upgrade indistinguishable from a downgrade. The type is therefore declared without those derives and the comparison has nothing to borrow. This is the STRUCTURAL half; the ordering itself is witnessed at runtime through `semantic_precedence`, and no mutation can witness the absence of a derive, because restoring it does not fail any test that does not already exist |
//! | PC2 (usage v2) | `57_author_v2_legacy_input` | the shipped v1 `inputTokens` ambiguity is a decoder/migration-only type and cannot be authored into a new `AttemptUsage` v2 value |
//!
//! Probes are split one **error phase** per file. rustc stops after a name
//! resolution or privacy failure and never reaches borrow checking or trait
//! selection, so two laws in one file silently witness only the first. Every
//! `.stderr` here was checked individually to confirm it names the error the
//! probe intends.
//!
//! Row 11 has two halves. The injection half is a compile probe:
//! `DeterministicAuthorities` has no `Default` and no zero-argument
//! constructor, so a durable owner that forgot its authorities does not build.
//! The ambient half CANNOT be a trybuild probe — `SystemTime::now` is valid
//! Rust and rustc will never reject it. It is enforced by
//! `clippy::disallowed_methods`, configured in the workspace `clippy.toml`,
//! and PROVEN to fire by a negative fixture crate that is EXCLUDED from the
//! workspace:
//!
//! ```sh
//! CLIPPY_CONF_DIR=. cargo clippy //!   --manifest-path fixtures/negative/ambient-clock/Cargo.toml -- -D warnings
//! ```
//!
//! must fail naming `clippy::disallowed_methods`. It used to be a
//! `probe-ambient-clock` Cargo feature on this crate, which was wrong twice:
//! features are additive, so any dependent could switch the forbidden call back
//! on, and it made `cargo clippy --workspace --all-targets --all-features --
//! -D warnings` permanently un-green — the fixture and the gate it proves were
//! in conflict.
//!
//! must fail naming `clippy::disallowed_methods`. `IdToken::new` stays public
//! because externally implemented `IdAuthority` adapters must construct what
//! they return; it is an inert output value and no domain, receipt, grant,
//! lease or settlement constructor accepts one (see `id_token_is_inert`).
//!
//! Row 10 is covered only in part, and the gap is recorded here rather than
//! silently skipped. F1A owns the identity separation between
//! `ToolDefinitionId` and `ToolSchemaVersion`, which the probe witnesses. The
//! remaining clause — that a tool or provider schema cannot be constructed
//! independently of its authoritative Rust argument/request type — has no
//! subject to probe yet: no schema type exists in any crate. Its owner is the
//! tool/schema lane (survey row G3), which must add the probe when the
//! authoritative argument type and its generated projection land.

#[test]
fn ownership_probes_are_rejected_by_the_compiler() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}

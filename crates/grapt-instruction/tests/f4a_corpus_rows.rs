//! F4A fixture runner over the two frozen corpus rows this lane owns.
//!
//! The rows live in the grapt repository, which is a separate repo from this
//! workspace. Point `SECODE_FIXTURES` at `<grapt>/fixtures/rust-reconstruction`
//! to override the default. Until the paired fixture candidate lands, that
//! override is how the pair is verified together — the Rust candidate and the
//! fixture candidate are accepted as a pair and neither is meaningful alone.
//!
//! This runner deliberately FAILS LOUDLY when a row is absent rather than
//! skipping. A fixture runner with no fixtures is a broken gate, and a gate
//! that passes silently is worse than one that fails.
//!
//! What it actually executes, so the rows are evidence rather than prose:
//!
//! * `fork-lineage-capped-reference` — the fixture's own `parentSeq` builds a
//!   real [`ForkBoundary`], the declared ancestry is rebuilt through the
//!   promoted walk and compared segment by segment, and every sequence the row
//!   declares inherited or excluded is checked against the resulting cap. The
//!   exclusion list is read FROM the row, so widening the cap in code without
//!   editing the row turns this red.
//! * `compaction-no-progress-typed-outcome` — each declared attempt is
//!   constructed for real and its outcome compared with the row's recorded
//!   result and exact typed failure.

use std::fs;
use std::path::{Path, PathBuf};

use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CompactionDescriptor, CompactionDescriptorParts,
    CompactionId, CompactionSummary, CompactionTrigger, ContextEpoch, ContextFailure,
    ContextWindowId, EventSeq, ForkBoundary, ForkLineageBuilder, LineageCap, NextRequestRef,
    ParentSeq, RetainedHistory, RetentionPolicyVersion, SessionId, StepNumber,
};
use serde_json::Value;
#[path = "../../../test-support/fixtures.rs"]
mod fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixtures_root() -> PathBuf {
    fixtures::fixture_path(Path::new(""))
}

fn read_json(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    assert!(
        path.exists(),
        "frozen F4A corpus row missing at {}. The Rust and fixture candidates are a pair; \
         set SECODE_FIXTURES to the fixture candidate root to verify them together.",
        path.display()
    );
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    assert!(
        path.exists(),
        "frozen F4A corpus row missing at {}",
        path.display()
    );
    let text = fs::read_to_string(path)?;
    let mut rows = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        rows.push(serde_json::from_str(line)?);
    }
    Ok(rows)
}

fn str_at<'a>(value: &'a Value, key: &str) -> Result<&'a str, Box<dyn std::error::Error>> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("row is missing string field {key}").into())
}

fn i64_at(value: &Value, key: &str) -> Result<i64, Box<dyn std::error::Error>> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("row is missing integer field {key}").into())
}

// ---- fork-lineage-capped-reference -------------------------------------

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn the_capped_fork_row_rebuilds_its_declared_ancestry_and_exclusions() -> TestResult {
    let row = fixtures_root().join("fork-lineage-capped-reference");
    let projection = read_json(&row.join("expected/projection.json"))?;
    let actions = read_jsonl(&row.join("actions.jsonl"))?;

    // The boundary comes from the row's own fork action, not from a constant
    // restated here. A row that changed its parentSeq would change this walk.
    let fork_action = actions
        .iter()
        .find(|action| action.get("type").and_then(Value::as_str) == Some("session.forked"))
        .ok_or("the row declares no session.forked action")?;
    let data = fork_action.get("data").ok_or("fork action has no data")?;
    let child: SessionId = str_at(data, "sessionID")?.parse()?;
    let parent: SessionId = str_at(data, "parentID")?.parse()?;
    let boundary =
        ForkBoundary::from_raw_parent_seq(child.clone(), parent, i64_at(data, "parentSeq")?)?;

    let child_projection = projection
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or("projection has no sessions")?
        .iter()
        .find(|session| session.get("id").and_then(Value::as_str) == Some(child.as_str()))
        .ok_or("projection has no child session")?;

    // Rebuild the ancestry the row declares, through the promoted walk.
    let mut builder = ForkLineageBuilder::own(child.clone());
    builder.descend(&boundary)?;
    let lineage = builder.finish();

    let declared = child_projection
        .get("lineage")
        .and_then(Value::as_array)
        .ok_or("the child projection declares no lineage")?;
    assert_eq!(
        lineage.segments().len(),
        declared.len(),
        "the rebuilt ancestry has a different length than the row declares"
    );
    for (segment, declared) in lineage.segments().iter().zip(declared) {
        assert_eq!(
            segment.session().as_str(),
            str_at(declared, "session_id")?,
            "ancestry order must match the row, oldest ancestor first"
        );
        let declared_cap = declared
            .get("cap")
            .ok_or("a lineage entry declares no cap")?;
        let expected_cap = if declared_cap.as_str() == Some("own") {
            LineageCap::Own
        } else {
            let through = declared_cap
                .get("inherited_through")
                .and_then(Value::as_i64)
                .ok_or("a lineage cap is neither \"own\" nor an inherited_through bound")?;
            LineageCap::Inherited(
                ParentSeq::from_raw(through).ok_or("declared cap is below the frozen floor")?,
            )
        };
        assert_eq!(
            segment.cap(),
            expected_cap,
            "cap mismatch for {}",
            segment.session()
        );
    }

    // Every sequence the row says is EXCLUDED must be refused by the cap that
    // applies to the aggregate it belongs to.
    let parent_cap = lineage
        .segments()
        .iter()
        .find(|segment| segment.session() != &child)
        .ok_or("the rebuilt ancestry has no parent segment")?
        .cap();
    let excluded = child_projection
        .get("excluded_from_inheritance")
        .and_then(Value::as_array)
        .ok_or("the row declares no exclusions")?;
    assert!(
        !excluded.is_empty(),
        "an exclusion list with no entries would make this row vacuous"
    );
    for entry in excluded {
        let seq = u64::try_from(i64_at(entry, "sequence")?)?;
        assert!(
            !parent_cap.admits(EventSeq::new(seq)),
            "the row declares parent sequence {seq} excluded ({}), but the cap admits it",
            str_at(entry, "reason").unwrap_or("no reason given")
        );
    }

    // ...and the boundary itself must still be inherited, or "excluded" would
    // be trivially satisfied by a cap that admits nothing.
    if let ParentSeq::Through(through) = boundary.parent_seq() {
        assert!(
            parent_cap.admits(through),
            "the cap must still inherit the boundary sequence itself"
        );
        assert!(
            parent_cap.admits(EventSeq::FIRST),
            "the cap must still inherit the parent's first fact"
        );
    }
    Ok(())
}

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn the_capped_fork_row_durable_log_matches_the_declared_boundary() -> TestResult {
    let row = fixtures_root().join("fork-lineage-capped-reference");
    let durable = read_jsonl(&row.join("expected/durable.jsonl"))?;
    let projection = read_json(&row.join("expected/projection.json"))?;

    let fork = durable
        .iter()
        .find(|fact| fact.get("fact").and_then(Value::as_str) == Some("forked"))
        .ok_or("the durable log declares no fork fact")?;
    let child = str_at(fork, "aggregate_id")?;
    let parent = str_at(fork, "parent_id")?;
    let parent_seq = i64_at(fork, "parent_seq")?;

    // The fork must be the child's first aggregate fact — the same law
    // `project_fork` enforces on the durable path.
    assert_eq!(
        i64_at(fork, "sequence")?,
        0,
        "session.forked must be the child's first aggregate event"
    );

    // The row must actually contain a parent fact ABOVE the cap and one
    // appended AFTER the fork, or it cannot witness either direction.
    let parent_seqs: Vec<i64> = durable
        .iter()
        .filter(|fact| fact.get("aggregate_id").and_then(Value::as_str) == Some(parent))
        .filter_map(|fact| fact.get("sequence").and_then(Value::as_i64))
        .collect();
    assert!(
        parent_seqs.iter().any(|seq| *seq <= parent_seq),
        "the row must contain at least one inherited parent fact at or below the cap"
    );
    assert!(
        parent_seqs.iter().any(|seq| *seq > parent_seq),
        "the row must contain at least one parent fact above the cap, or the cap witnesses nothing"
    );

    // The child's own post-fork fact must be present, or "own-aggregate truth"
    // is unwitnessed.
    assert!(
        durable.iter().any(|fact| {
            fact.get("aggregate_id").and_then(Value::as_str) == Some(child)
                && fact.get("sequence").and_then(Value::as_i64).unwrap_or(0) > 0
        }),
        "the row must contain a child-owned fact after the fork"
    );

    // The declared exclusions must name real parent sequences from this log.
    let child_projection = projection
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or("projection has no sessions")?
        .iter()
        .find(|session| session.get("id").and_then(Value::as_str) == Some(child))
        .ok_or("projection has no child session")?;
    for entry in child_projection
        .get("excluded_from_inheritance")
        .and_then(Value::as_array)
        .ok_or("the row declares no exclusions")?
    {
        let seq = i64_at(entry, "sequence")?;
        assert!(
            parent_seqs.contains(&seq),
            "the row excludes parent sequence {seq}, which its own durable log does not contain"
        );
        assert!(
            seq > parent_seq,
            "the row excludes parent sequence {seq}, which is at or below the cap and must be inherited"
        );
    }
    Ok(())
}

// ---- compaction-no-progress-typed-outcome ------------------------------

fn artifact(name: &str) -> Result<ArtifactRef, Box<dyn std::error::Error>> {
    Ok(ArtifactRef::new(
        ArtifactId::new(name).ok_or("artifact id")?,
        12,
        ArtifactDigest::new("digest").ok_or("artifact digest")?,
    ))
}

fn parts_from(declared: &Value) -> Result<CompactionDescriptorParts, Box<dyn std::error::Error>> {
    Ok(CompactionDescriptorParts {
        id: CompactionId::new(str_at(declared, "id")?).ok_or("compaction id")?,
        source_session: str_at(declared, "source_session")?.parse()?,
        source_epoch: ContextEpoch::new(u64::try_from(i64_at(declared, "source_epoch")?)?),
        target_window: ContextWindowId::new(str_at(declared, "target_window")?)
            .ok_or("context window id")?,
        target_epoch: ContextEpoch::new(u64::try_from(i64_at(declared, "target_epoch")?)?),
        trigger: match str_at(declared, "trigger")? {
            "context_overflow" => CompactionTrigger::ContextOverflow,
            "policy_threshold" => CompactionTrigger::PolicyThreshold,
            "user_requested" => CompactionTrigger::UserRequested,
            "emergency_headroom" => CompactionTrigger::EmergencyHeadroom,
            other => return Err(format!("row declares unknown trigger {other}").into()),
        },
        retention_policy: RetentionPolicyVersion::new(str_at(declared, "retention_policy")?)
            .ok_or("retention policy version")?,
        // F4B1 added the §4.2 next-request reassembly reference as a REQUIRED
        // coordinate. The frozen F4A row predates it and cannot declare one, and
        // the fixture candidate is frozen, so this row supplies a local value.
        // That is sound for what this row asserts: it exercises the no-progress
        // refusal, which `CompactionDescriptor::new` decides from the epochs
        // before any other coordinate is read, so the reference cannot change
        // the outcome either way. Recorded as a residual so a future fixture
        // revision can declare it.
        next_request: NextRequestRef::new(
            StepNumber::new(1).ok_or("step number")?,
            artifact("art-next-request")?,
        ),
        summary: CompactionSummary::ClientReadable {
            artifact: artifact("art-summary")?,
        },
        retained: RetainedHistory::References {
            artifacts: vec![artifact("art-retained")?],
        },
        baseline: artifact("art-baseline")?,
    })
}

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn the_no_progress_row_reproduces_every_declared_outcome() -> TestResult {
    let row = fixtures_root().join("compaction-no-progress-typed-outcome");
    let actions = read_jsonl(&row.join("actions.jsonl"))?;
    let state = read_json(&row.join("expected/state.json"))?;

    let outcomes = state
        .get("outcomes")
        .and_then(Value::as_array)
        .ok_or("the row declares no outcomes")?;
    assert_eq!(
        actions.len(),
        outcomes.len(),
        "every declared action must have exactly one declared outcome"
    );

    let mut refused = 0_usize;
    let mut committed = 0_usize;
    for action in &actions {
        let ordinal = i64_at(action, "ordinal")?;
        let declared = action
            .get("descriptor")
            .ok_or("an action declares no descriptor")?;
        let outcome = outcomes
            .iter()
            .find(|outcome| outcome.get("action_ordinal").and_then(Value::as_i64) == Some(ordinal))
            .ok_or("an action has no matching outcome")?;

        match CompactionDescriptor::new(parts_from(declared)?) {
            Ok(descriptor) => {
                committed += 1;
                assert_eq!(
                    str_at(outcome, "result")?,
                    "committed",
                    "action {ordinal} committed a descriptor the row says is refused"
                );
                assert!(
                    descriptor.target_epoch() > descriptor.source_epoch(),
                    "a committed descriptor must advance the epoch"
                );
            }
            Err(failure) => {
                refused += 1;
                assert_eq!(
                    str_at(outcome, "result")?,
                    "refused",
                    "action {ordinal} was refused but the row says it commits"
                );
                assert_eq!(
                    failure,
                    ContextFailure::CompactionNoProgress,
                    "action {ordinal} must be refused as no-progress, not another context failure"
                );
                assert_eq!(
                    str_at(outcome, "failure")?,
                    "ContextFailure::CompactionNoProgress",
                    "the row must name the exact typed failure it observed"
                );
            }
        }
    }

    // A row of only refusals would pass against a constructor that refuses
    // everything, and a row of only commits would witness no law at all.
    assert!(
        refused >= 2,
        "the row must witness both the stalled and the backwards epoch"
    );
    assert!(
        committed >= 1,
        "the row must witness an accepted advancing compaction, or a constructor \
         that refuses unconditionally would satisfy it"
    );
    Ok(())
}

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn the_no_progress_row_commits_no_durable_fact() -> TestResult {
    let row = fixtures_root().join("compaction-no-progress-typed-outcome");
    let durable = read_jsonl(&row.join("expected/durable.jsonl"))?;
    assert!(
        durable.is_empty(),
        "a refused compaction must commit nothing; the row declared {} durable facts",
        durable.len()
    );
    Ok(())
}

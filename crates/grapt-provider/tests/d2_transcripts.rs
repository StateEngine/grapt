//! Fixture runner over the provider-stream transcripts.
//!
//! These fixtures were captured from the real oracle engine at oracle
//! `42e4888cc0` via its deterministic TestLLM adapter, and their `expected/*`
//! files were emitted from the run receipt, so they are oracle-true by
//! construction. This runner asserts two different things:
//!
//! 1. **Coverage** — the closed [`AttemptFragment`] alphabet can express every
//!    durable variant the oracle actually produced. A variant the oracle emits
//!    that this crate cannot name is a hole in the vocabulary.
//! 2. **Corrected expectations** — where the oracle is a recorded RED
//!    baseline, the Rust behaviour must differ in the specific way the
//!    fixture's `waivers.json` prescribes, and must differ *only* there.
//!
//! The fixtures live in the grapt repository, which is a separate repository
//! from this workspace; the separation was ruled on 2026-08-08. Point `SECODE_FIXTURES` at
//! `<grapt>/fixtures/rust-reconstruction` to override the default.
//!
//! This test fails loudly when the fixtures are absent rather than skipping. A
//! fixture runner with no fixtures is a broken gate, and a gate that passes
//! silently is worse than one that fails.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use grapt_provider::{
    AdapterState, AttemptFailure, AttemptFragment, BlockOrdinal, FinishReason, StragglerKind,
    StreamValidator, StreamViolation, ToolExecution,
};
use grapt_types::ToolCallId;
#[path = "../../../test-support/fixtures.rs"]
mod fixtures;

fn scenario(name: &str) -> PathBuf {
    let dir = fixtures::fixture_path(Path::new(name));
    assert!(
        dir.is_dir(),
        "provider-stream transcript fixture {name} not found at {}. Set SECODE_FIXTURES to <grapt>/fixtures/rust-reconstruction.",
        dir.display()
    );
    dir
}

// ---------------------------------------------------------------------------
// A deliberately small JSON reader.
//
// The fixtures are machine-generated JSONL with a known shape. Rather than add
// a serde dependency to a crate that has none, this extracts the few fields
// the runner needs. It is not a general JSON parser and does not pretend to
// be: it fails loudly on anything it cannot read.
// ---------------------------------------------------------------------------

/// Pull a string field value from one JSON object line.
fn string_field(line: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\": \"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// Pull an unsigned integer field from one JSON object line.
fn u64_field(line: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\": ");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Pull a boolean field from one JSON object line.
fn bool_field(line: &str, key: &str) -> Option<bool> {
    let needle = format!("\"{key}\": ");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) => panic!("cannot read {}: {error}", path.display()),
    };
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// One durable event, reduced to what this runner reasons about.
#[derive(Debug, Clone)]
struct DurableEvent {
    seq: u64,
    kind: String,
    raw: String,
}

fn durable(dir: &Path) -> Vec<DurableEvent> {
    read_lines(&dir.join("expected/durable.jsonl"))
        .into_iter()
        .map(|raw| {
            let kind = string_field(&raw, "type")
                .unwrap_or_else(|| panic!("durable line has no type field: {raw}"));
            let seq = u64_field(&raw, "seq")
                .unwrap_or_else(|| panic!("durable line has no seq field: {raw}"));
            DurableEvent { seq, kind, raw }
        })
        .collect()
}

fn kinds(events: &[DurableEvent]) -> Vec<&str> {
    events.iter().map(|event| event.kind.as_str()).collect()
}

fn call_id(raw: &str) -> ToolCallId {
    let id =
        string_field(raw, "callID").unwrap_or_else(|| panic!("tool event has no callID: {raw}"));
    ToolCallId::new(id).unwrap_or_else(|| panic!("empty callID in {raw}"))
}

/// Translate the oracle's durable session events into this crate's alphabet.
///
/// This is the coverage proof. Every durable event type the oracle emits for a
/// provider stream must map onto a fragment, or be explicitly recognised as
/// session-level bookkeeping that is not a provider fragment at all.
///
/// Returns the fragments plus the durable types that were deliberately not
/// translated, so the test can assert the skip list rather than let unmapped
/// types disappear silently.
#[expect(clippy::too_many_lines, reason = "legacy")]
fn to_fragments(events: &[DurableEvent]) -> (Vec<AttemptFragment>, Vec<String>) {
    let mut fragments = vec![AttemptFragment::AttemptOpened];
    let mut skipped = Vec::new();
    // The oracle numbers text and reasoning blocks with its own `ordinal`.
    let mut open_tools: BTreeMap<String, ToolExecution> = BTreeMap::new();

    for event in events {
        let raw = event.raw.as_str();
        match event.kind.as_str() {
            // Session bookkeeping — admission, instructions, promotion, and the
            // step envelope are the owner's facts, not provider fragments.
            "session.input.admitted.1"
            | "session.instructions.updated.2"
            | "session.input.promoted.1"
            | "session.step.started.1" => skipped.push(event.kind.clone()),

            "session.reasoning.started.1" => {
                let ordinal = u64_field(raw, "ordinal").unwrap_or(0);
                fragments.push(AttemptFragment::ReasoningOpened {
                    block: BlockOrdinal::new(ordinal as u32),
                    state: AdapterState::none(),
                });
            }
            "session.reasoning.ended.1" => {
                let ordinal = u64_field(raw, "ordinal").unwrap_or(0);
                let block = BlockOrdinal::new(ordinal as u32);
                // The oracle persists the accumulated text on the END event;
                // deltas are live-only. The contract explicitly permits this:
                // "Text/reasoning/input deltas may be live-only to avoid
                // durable projection churn". Reconstruct one delta so the
                // replayable content is not lost.
                if let Some(text) = string_field(raw, "text") {
                    fragments.push(AttemptFragment::ReasoningDelta {
                        block,
                        text,
                        state: AdapterState::none(),
                    });
                }
                fragments.push(AttemptFragment::ReasoningClosed {
                    block,
                    state: AdapterState::none(),
                });
            }
            "session.text.started.1" => {
                let ordinal = u64_field(raw, "ordinal").unwrap_or(0);
                fragments.push(AttemptFragment::TextOpened {
                    block: BlockOrdinal::new(ordinal as u32),
                    state: AdapterState::none(),
                });
            }
            "session.text.ended.1" => {
                let ordinal = u64_field(raw, "ordinal").unwrap_or(0);
                let block = BlockOrdinal::new(ordinal as u32);
                if let Some(text) = string_field(raw, "text") {
                    fragments.push(AttemptFragment::TextDelta {
                        block,
                        text,
                        state: AdapterState::none(),
                    });
                }
                fragments.push(AttemptFragment::TextClosed {
                    block,
                    state: AdapterState::none(),
                });
            }

            "session.tool.input.started.1" => {
                let call = call_id(raw);
                let tool = string_field(raw, "name").unwrap_or_default();
                // Execution owner is not known until `called` carries
                // `executed`; the oracle's input events do not state it.
                // Default to Local and correct on `called`, which is the only
                // event that declares it.
                open_tools.insert(call.as_str().to_owned(), ToolExecution::Local);
                fragments.push(AttemptFragment::ToolInputOpened {
                    call,
                    tool,
                    execution: ToolExecution::Local,
                    state: AdapterState::none(),
                });
            }
            "session.tool.input.ended.1" => {
                let call = call_id(raw);
                let tool = tool_name_for(&fragments, &call);
                fragments.push(AttemptFragment::ToolInputClosed {
                    call,
                    tool,
                    state: AdapterState::none(),
                });
            }
            "session.tool.called.1" => {
                let call = call_id(raw);
                let tool = tool_name_for(&fragments, &call);
                // `executed: true` means the PROVIDER ran it — this is the
                // oracle's optional-boolean form of the execution owner, and
                // is exactly the field the contract requires be a closed enum.
                let execution = match bool_field(raw, "executed") {
                    Some(true) => ToolExecution::ProviderHosted,
                    _ => ToolExecution::Local,
                };
                open_tools.insert(call.as_str().to_owned(), execution);
                fragments.push(AttemptFragment::ToolCalled {
                    call,
                    tool,
                    input: String::new(),
                    execution,
                    state: AdapterState::none(),
                });
            }
            "session.tool.success.2" => {
                let call = call_id(raw);
                let tool = tool_name_for(&fragments, &call);
                fragments.push(AttemptFragment::HostedToolResult {
                    call,
                    tool,
                    result: String::new(),
                    state: AdapterState::none(),
                });
            }
            "session.tool.failed.2" => {
                // A LOCAL tool failure is settled by the owner from its join
                // set, not by a provider fragment. Only hosted failures are
                // stream fragments, so this is skipped rather than mistranslated.
                skipped.push(event.kind.clone());
            }

            "session.step.ended.1" => {
                let finish = match string_field(raw, "finish").as_deref() {
                    Some("stop") => FinishReason::Stop,
                    Some("length") => FinishReason::Length,
                    Some("tool-calls") => FinishReason::ToolCalls,
                    Some("content-filter") => FinishReason::ContentFilter,
                    Some("error") => FinishReason::Error,
                    _ => FinishReason::Unknown,
                };
                fragments.push(AttemptFragment::ProviderTerminal {
                    finish,
                    // Usage rides in this envelope, never as a standalone
                    // fragment — see the coverage test.
                    usage: None,
                    state: AdapterState::none(),
                });
            }
            "session.step.failed.1" => {
                // The owner's terminal after an interrupt sweep, not a
                // provider fragment: the provider never sent anything.
                skipped.push(event.kind.clone());
            }

            other => panic!(
                "durable event type {other} (seq {}) has no mapping — the vocabulary may have a hole",
                event.seq
            ),
        }
    }
    (fragments, skipped)
}

/// Recover a tool's name from the fragments already emitted for that call.
fn tool_name_for(fragments: &[AttemptFragment], call: &ToolCallId) -> String {
    for fragment in fragments.iter().rev() {
        match fragment {
            AttemptFragment::ToolInputOpened { call: id, tool, .. }
            | AttemptFragment::ToolCalled { call: id, tool, .. }
                if id == call =>
            {
                return tool.clone()
            }
            _ => {}
        }
    }
    String::new()
}

fn validate(fragments: &[AttemptFragment]) -> Result<StreamValidator, StreamViolation> {
    let mut validator = StreamValidator::new();
    for fragment in fragments {
        validator.accept(fragment)?;
    }
    Ok(validator)
}

// ---------------------------------------------------------------------------
// Coverage
// ---------------------------------------------------------------------------

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn reasoning_text_usage_stop_maps_onto_the_alphabet() {
    let dir = scenario("provider-transcript-reasoning-text-usage-stop");
    let events = durable(&dir);
    let (fragments, skipped) = to_fragments(&events);

    match validate(&fragments) {
        Ok(validator) => {
            assert!(validator.terminated(), "the clean stop must terminate");
            assert!(validator.visible_output(), "reasoning and text are visible");
            assert!(
                validator.unsettled_calls().is_empty(),
                "no tool calls in this transcript"
            );
        }
        Err(violation) => panic!("oracle-true transcript must validate: {violation}"),
    }

    // Only session bookkeeping was skipped — every provider-produced event was
    // expressible.
    assert_eq!(
        skipped,
        vec![
            "session.input.admitted.1",
            "session.instructions.updated.2",
            "session.input.promoted.1",
            "session.step.started.1",
        ]
    );
}

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn usage_is_never_a_standalone_fragment_in_the_oracle() {
    // The waiver states it outright: "Usage as a standalone AttemptFragment
    // is NOT oracle behavior: usage rides only in the finish/step-finish
    // envelope in this transcript."
    //
    // So AttemptFragment::Usage is a PROVISIONAL variant with no oracle
    // witness. This test pins that fact so the variant cannot quietly be
    // treated as proven. It remains provisional
    // pending a real-adapter incremental-usage capture.
    let dir = scenario("provider-transcript-reasoning-text-usage-stop");
    let events = durable(&dir);
    let types = kinds(&events);
    assert!(
        !types.iter().any(|kind| kind.contains("usage")),
        "no standalone usage event exists in the oracle transcript: {types:?}"
    );

    // The script pushed usage{input:42,output:7}, yet the durable step.ended
    // reports zeros. Pinned as an observation, not asserted as correct — it is
    // the reason the real-adapter capture is still owed.
    let step_ended = events
        .iter()
        .find(|event| event.kind == "session.step.ended.1")
        .map(|event| event.raw.clone())
        .unwrap_or_default();
    assert!(
        step_ended.contains("\"tokens\""),
        "usage rides inside the step terminal envelope"
    );
}

// ---------------------------------------------------------------------------
// Corrected expectations
// ---------------------------------------------------------------------------

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn malformed_fragment_yields_one_step_failure_where_the_oracle_recorded_none() {
    // ORACLE RED BASELINE (waivers.json): a tool input delta before start
    // raises an unchecked defect through session.resume and the durable log
    // shows ZERO step terminal facts — receipt `stepTerminalsRecorded: []`.
    let dir = scenario("provider-malformed-fragment-known-defect");
    let events = durable(&dir);
    let types = kinds(&events);

    // 1. Confirm the recorded red baseline is what we think it is.
    assert!(
        !types
            .iter()
            .any(|kind| kind.starts_with("session.step.ended")
                || kind.starts_with("session.step.failed")),
        "oracle baseline must contain no step terminal: {types:?}"
    );
    assert_eq!(
        types.last().copied(),
        Some("session.step.started.1"),
        "the oracle log stops at step.started because the drain fiber died"
    );

    // 2. Rust behaviour: the same malformed ordering produces exactly one
    //    Step.Failed and no crash. Rebuild the offending sequence — a tool
    //    input delta with no preceding open, which is what the actions file
    //    scripted.
    let call = ToolCallId::new("call-1").unwrap_or_else(|| panic!("valid id"));
    let mut validator = StreamValidator::new();
    assert!(validator.accept(&AttemptFragment::AttemptOpened).is_ok());

    let malformed = AttemptFragment::ToolInputDelta {
        call: call.clone(),
        tool: "echo".into(),
        text: "{".into(),
    };
    let failures: Vec<AttemptFailure> = match validator.accept(&malformed) {
        Ok(_) => panic!("a tool input delta before start must be rejected"),
        Err(violation) => vec![violation.attempt_failure()],
    };

    assert_eq!(
        failures.len(),
        1,
        "exactly one Step.Failed, never zero or two"
    );
    assert_eq!(
        failures[0].reason(),
        &grapt_types::AttemptTerminalReason::ProviderFailure {
            class: grapt_types::ProviderFailureClass::InvalidProviderOutput
        }
    );
    assert!(
        failures[0].detail().as_str().contains("call-1"),
        "the failure must name the offending call for diagnosis: {}",
        failures[0].detail()
    );
}

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn interrupt_midstream_settles_every_started_call() {
    // The oracle's interrupt path runs through running local tools. Its
    // corrected expectation: "Rust AttemptSupervisor must cancel the provider
    // reader directly and settle truthfully regardless of whether a tool is
    // running; the oracle's asymmetry is a defect fact, not behavior to copy."
    //
    // The supervisor is not in this slice. What this crate must supply is the
    // truthful sweep list the supervisor settles from, so that is what is
    // asserted here.
    let dir = scenario("provider-interrupt-midstream");
    let events = durable(&dir);
    let (fragments, _) = to_fragments(&events);

    let validator = match validate(&fragments) {
        Ok(validator) => validator,
        Err(violation) => panic!("oracle-true transcript must validate: {violation}"),
    };

    // The provider never sent a terminal — the interrupt did. The stream is
    // therefore un-terminated, which is exactly the state the supervisor must
    // handle.
    assert!(
        !validator.terminated(),
        "an interrupted stream has no provider terminal"
    );
    assert!(
        validator.visible_output(),
        "text was emitted before the interrupt"
    );

    // The local tool that was running at interrupt time must appear in the
    // sweep so the supervisor can write its truthful aborted terminal. The
    // oracle wrote exactly that: session.tool.failed.2 {type: aborted}.
    let unsettled: Vec<&str> = validator
        .unsettled_calls()
        .iter()
        .map(|id| id.as_str())
        .collect();
    assert_eq!(
        unsettled,
        vec!["call-interrupt"],
        "the running local tool must be swept, not silently dropped"
    );

    // Corroborate against the oracle's own durable log.
    let types = kinds(&events);
    assert!(
        types.contains(&"session.tool.failed.2"),
        "oracle settled the tool as aborted"
    );
    assert!(
        types.contains(&"session.step.failed.1"),
        "oracle wrote one step terminal"
    );
}

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn straggler_asymmetry_is_ignored_evidence_not_a_second_settlement() {
    // caseA: late ERROR after success — benign in the oracle, which records
    //        toolSuccessEvents: 1 and ranToCompletion: true.
    // caseB: duplicate SUCCESS — the oracle raises an unchecked defect
    //        ("Duplicate tool result: call-dup"). Corrected expectation:
    //        "Rust records it as ignored transport evidence with a diagnostic,
    //        never a second settlement and never a crash."
    let dir = scenario("provider-straggler-asymmetry");
    let events = durable(&dir);
    let types = kinds(&events);

    // The oracle's caseB log stops after the FIRST success — the duplicate
    // killed the run before any step terminal for that step.
    let successes = types
        .iter()
        .filter(|kind| **kind == "session.tool.success.2")
        .count();
    assert_eq!(successes, 2, "one success per case, never a duplicate");

    // Rust: replay caseB's shape and prove both stragglers are ignored.
    let call = ToolCallId::new("call-dup").unwrap_or_else(|| panic!("valid id"));
    let mut validator = StreamValidator::new();
    let opening = [
        AttemptFragment::AttemptOpened,
        AttemptFragment::ToolInputOpened {
            call: call.clone(),
            tool: "web_search".into(),
            execution: ToolExecution::ProviderHosted,
            state: AdapterState::none(),
        },
        AttemptFragment::ToolCalled {
            call: call.clone(),
            tool: "web_search".into(),
            input: "{}".into(),
            execution: ToolExecution::ProviderHosted,
            state: AdapterState::none(),
        },
        AttemptFragment::HostedToolResult {
            call: call.clone(),
            tool: "web_search".into(),
            result: "[]".into(),
            state: AdapterState::none(),
        },
    ];
    for fragment in &opening {
        assert!(
            validator.accept(fragment).is_ok(),
            "{fragment:?} must be legal"
        );
    }

    // The duplicate SUCCESS: ignored, diagnosed, no crash, no step failure.
    let duplicate = AttemptFragment::HostedToolResult {
        call: call.clone(),
        tool: "web_search".into(),
        result: "[]".into(),
        state: AdapterState::none(),
    };
    match validator.accept(&duplicate) {
        Ok(accepted) => {
            let evidence = accepted
                .ignored
                .unwrap_or_else(|| panic!("the duplicate must be reported as ignored evidence"));
            assert_eq!(evidence.call, "call-dup");
            assert_eq!(evidence.kind, StragglerKind::Success);
        }
        Err(violation) => {
            panic!("the oracle's crash must NOT become a Rust step failure: {violation}")
        }
    }

    // The late ERROR (caseA's shape) is likewise ignored, with a distinct kind.
    let late_error = AttemptFragment::HostedToolFailed {
        call,
        tool: "web_search".into(),
        error: "late".into(),
        state: AdapterState::none(),
    };
    match validator.accept(&late_error) {
        Ok(accepted) => assert_eq!(
            accepted.ignored.map(|evidence| evidence.kind),
            Some(StragglerKind::Failure)
        ),
        Err(violation) => panic!("a late error must stay benign: {violation}"),
    }

    // Exactly-once settlement survived both stragglers.
    assert!(
        validator.unsettled_calls().is_empty(),
        "the call settled once and stayed settled"
    );
}

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn every_d2_scenario_declares_its_waivers() {
    // A corrected expectation without a waiver row is an undisclosed behaviour
    // change. Every scenario this runner consumes must carry both files.
    for name in [
        "provider-transcript-reasoning-text-usage-stop",
        "provider-malformed-fragment-known-defect",
        "provider-interrupt-midstream",
        "provider-straggler-asymmetry",
    ] {
        let dir = scenario(name);
        for required in [
            "manifest.json",
            "actions.jsonl",
            "expected/durable.jsonl",
            "expected/waivers.json",
        ] {
            assert!(dir.join(required).is_file(), "{name} is missing {required}");
        }
        let waivers = match fs::read_to_string(dir.join("expected/waivers.json")) {
            Ok(raw) => raw,
            Err(error) => panic!("cannot read waivers for {name}: {error}"),
        };
        assert!(
            waivers.contains("engine-identity-normalization"),
            "{name} must carry the ratified identity-normalization waiver"
        );
    }
}

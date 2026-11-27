use super::*;
use grapt_artifact::{ArtifactKind, CaptureBound, Sensitivity};
use grapt_types::{
    CompactionDescriptor, CompactionDescriptorParts, CompactionId, CompactionTrigger, ContextEpoch,
    ContextWindowId, NextRequestRef, RetentionPolicyVersion, StepNumber,
};

#[test]
fn scc_selection_rejects_corrupt_coordinates_before_history_lookup() -> Result<(), String> {
    let good = serde_json::json!({"session":"ses_scc_selection", "input":"msg_scc_selection",
        "compaction":"scc-selection", "from":1, "source_head":2});
    assert_eq!(Selection::decode(&good.to_string())?.encode(), good);
    for (field, value) in [
        ("session", serde_json::json!("msg_wrong_family")),
        ("input", serde_json::json!("ses_wrong_family")),
        ("compaction", serde_json::json!("x".repeat(257))),
        ("from", serde_json::json!(-1)),
        ("source_head", serde_json::Value::Null),
        ("extra", serde_json::json!(true)),
    ] {
        let mut bad = good.clone();
        bad[field] = value;
        assert!(
            Selection::decode(&bad.to_string()).is_err(),
            "accepted corrupt {field}"
        );
    }
    let diagnosed = Selection {
        session: "ses_scc_selection"
            .parse::<SessionId>()
            .map_err(|error| error.to_string())?,
        input: "msg_scc_selection"
            .parse::<MessageId>()
            .map_err(|error| error.to_string())?,
        compaction: CompactionId::new("scc-selection").ok_or("compaction")?,
        from: 1,
        source_head: 2,
        admission: Some(AdmissionDiagnosis {
            trigger: CompactionTrigger::PolicyThreshold,
            estimate_basis: "confirmed-prefix",
            estimate: 114_000,
            rebuilt_estimate: Some(22_000),
            budget: 128_000,
            removed_bytes: 80_000,
            retained_bytes: 34_000,
        }),
        server: None,
        summary_plan: None,
    }
    .encode();
    assert_eq!(
        Selection::decode(&diagnosed.to_string())?.encode(),
        diagnosed
    );
    let mut legacy = diagnosed.clone();
    legacy["admission"]
        .as_object_mut()
        .ok_or("diagnosed admission is not an object")?
        .remove("rebuiltEstimate");
    assert_eq!(Selection::decode(&legacy.to_string())?.encode(), legacy);
    for (field, value) in [
        ("trigger", serde_json::json!("guess")),
        ("estimateBasis", serde_json::json!("assumed")),
        ("estimate", serde_json::json!(-1)),
        ("rebuiltEstimate", serde_json::json!(-1)),
        ("budget", serde_json::Value::Null),
        ("removedBytes", serde_json::json!("many")),
        ("retainedBytes", serde_json::json!(-2)),
        ("extra", serde_json::json!(true)),
    ] {
        let mut bad = diagnosed.clone();
        bad["admission"][field] = value;
        assert!(
            Selection::decode(&bad.to_string()).is_err(),
            "accepted corrupt admission {field}"
        );
    }
    Ok(())
}

fn capture(
    artifacts: &FilesystemArtifactStore,
    text: &str,
) -> Result<grapt_types::ArtifactRef, String> {
    let bound = CaptureBound::new(MAX_SUMMARY_BYTES, 4096).ok_or("capture bound")?;
    let captured = artifacts
        .capture(
            std::io::Cursor::new(text.as_bytes()),
            bound,
            ArtifactKind::CapturedOutput,
            Sensitivity::Ordinary,
        )
        .map_err(|error| error.to_string())?;
    if !captured.disposition().is_complete() {
        return Err("fixture capture truncated".to_owned());
    }
    Ok(captured.reference().clone())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn scc_request_uses_only_committed_summary_and_preserves_input_authority() -> Result<(), String> {
    for bad_baseline in [false, true] {
        let (continuation, _scratch) = DurableContinuation::empty_for_test("scc-retained")?;
        let summary = capture(
            &continuation.artifacts,
            "Previous work: read the repository. Continue the same task.",
        )?;
        let selection = capture(
            &continuation.artifacts,
            &Selection {
                session: continuation.session.clone(),
                input: continuation.input.clone(),
                compaction: CompactionId::new("scc-request").ok_or("compaction")?,
                from: 1,
                source_head: 1,
                admission: Some(AdmissionDiagnosis {
                    trigger: CompactionTrigger::StructuralBound,
                    estimate_basis: "structural-window",
                    estimate: 129,
                    rebuilt_estimate: None,
                    budget: 128,
                    removed_bytes: 256,
                    retained_bytes: 128,
                }),
                server: None,
                summary_plan: None,
            }
            .encode()
            .to_string(),
        )?;
        // Artifacts alone are not a committed context boundary.
        assert_eq!(continuation.request_input("hello", false)?.len(), 1);
        let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
            id: CompactionId::new("scc-request").ok_or("compaction id")?,
            source_session: continuation.session.clone(),
            source_epoch: ContextEpoch::FIRST,
            target_window: ContextWindowId::new("scc-window").ok_or("window")?,
            target_epoch: ContextEpoch::new(1),
            trigger: CompactionTrigger::ContextOverflow,
            retention_policy: RetentionPolicyVersion::new(RETENTION_POLICY).ok_or("policy")?,
            summary: CompactionSummary::ClientReadable { artifact: summary },
            retained: RetainedHistory::DeterministicSelection {
                descriptor: selection.clone(),
            },
            baseline: if bad_baseline {
                capture(&continuation.artifacts, "unrelated baseline")?
            } else {
                selection.clone()
            },
            next_request: NextRequestRef::new(StepNumber::new(1).ok_or("step")?, selection),
        })
        .map_err(|error| format!("descriptor: {error:?}"))?;
        for publish in [false, true] {
            {
                let mut inbox = lock(&continuation.inbox);
                let aggregate = grapt_store::AggregateId::Session(continuation.session.clone());
                let held = inbox.store().held_writers();
                let tx = inbox
                    .store_mut()
                    .governed_transaction()
                    .map_err(|error| error.to_string())?;
                grapt_store::compaction::record(
                    &tx,
                    &"evt_scc_request"
                        .parse::<EventId>()
                        .map_err(|error| error.to_string())?,
                    held.token(&aggregate).map_err(|error| error.to_string())?,
                    1235,
                    grapt_store::compaction::CompactionAttempt::Settled(&descriptor),
                    &mut [],
                )
                .map_err(|error| error.to_string())?;
                if publish {
                    tx.commit().map_err(|error| error.to_string())?.discard();
                }
            }
            if !publish {
                assert_eq!(continuation.request_input("hello", false)?.len(), 1);
            }
        }
        if bad_baseline {
            assert!(continuation
                .request_input("hello", false)
                .is_err_and(|error| error.contains("baseline and request context disagree")));
            continue;
        }
        let (_, identity) = continuation.request_with_identity("hello", false)?;
        assert_eq!(
            identity.as_ref().map(|context| context.window.as_str()),
            Some("scc-window")
        );
        let input = continuation.request_input("hello", false)?;
        assert!(input
            .iter()
            .any(|item| matches!(item, CodexInputItem::Message(message)
                if message.role == MessageRole::Assistant
                    && message.content.starts_with(SUMMARY_LABEL)
                    && message.content.ends_with("Continue the same task."))));
        assert!(
            matches!(input.last(), Some(CodexInputItem::Message(message))
            if message.role == MessageRole::User && message.content == "hello")
        );
        assert!(continuation
            .request_input("changed without admission", false)
            .is_err());
    }
    Ok(())
}

/// SC-CACHE-07: every summary plan round-trips on the
/// selection, a selection written before the field still decodes, and an
/// unknown or non-string plan is refused rather than read as warm.
#[test]
fn cache07_the_summary_plan_round_trips_and_an_unknown_plan_is_refused() -> Result<(), String> {
    let good = serde_json::json!({"session":"ses_cache07_plan", "input":"msg_cache07_plan",
        "compaction":"cache07-plan", "from":1, "source_head":2});
    assert_eq!(Selection::decode(&good.to_string())?.summary_plan, None);
    for plan in SummaryPlan::ALL {
        let mut with = good.clone();
        with["summaryPlan"] = plan.name().into();
        let decoded = Selection::decode(&with.to_string())?;
        assert_eq!(decoded.summary_plan, Some(plan));
        assert_eq!(decoded.encode(), with);
    }
    assert_eq!(
        SummaryPlan::ColdHostedToolOffered.name(),
        "cold:hosted-tool-offered"
    );
    for bad in [
        serde_json::json!("cold:retry"),
        serde_json::json!("hot"),
        serde_json::json!(""),
        serde_json::json!(1),
        serde_json::Value::Null,
    ] {
        let mut with = good.clone();
        with["summaryPlan"] = bad.clone();
        assert!(
            Selection::decode(&with.to_string()).is_err(),
            "accepted summary plan {bad}"
        );
    }
    Ok(())
}

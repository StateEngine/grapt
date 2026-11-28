use super::*;

fn turn(body: &str, cost_usd: Option<f64>) -> PublishTurn {
    PublishTurn {
        idempotency_key: IdempotencyKey("key-fixture".into()),
        target_agent_id: None,
        session_id: None,
        conversation_id: "conversation-fixture".into(),
        harness: "harness-fixture".into(),
        model: TurnModel {
            provider: "provider-fixture".into(),
            model: "model-fixture".into(),
        },
        vendor_turn_id: "turn-1".into(),
        occurred_at_us: 1,
        role: TurnRole::Assistant,
        body_text: Some(body.into()),
        body_omitted_bytes: 0,
        content_sha256: Sha256Hex([0; 32]),
        usage: Some(TurnUsage {
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd,
        }),
        diff_summary: None,
        decision: None,
        nesting: None,
    }
}

#[test]
fn a_cost_that_json_cannot_carry_is_refused() {
    assert_eq!(turn("done", Some(0.25)).check(), Ok(()));
    let late = PublishTurn {
        occurred_at_us: -1,
        ..turn("done", None)
    };
    assert!(late.check().is_err());
    for cost in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(turn("done", Some(cost)).check().is_err());
    }
}

#[test]
fn prose_bounds_count_the_escaped_json() {
    // Under 1 MiB raw, over it once each quote is escaped.
    let quoted = "\"".repeat(MAX_PROSE_BYTES / 2 + 1);
    assert!(quoted.len() < MAX_PROSE_BYTES);
    assert!(turn(&quoted, None).check().is_err());
    assert_eq!(turn(&"a".repeat(MAX_PROSE_BYTES / 2), None).check(), Ok(()));
}

#[test]
fn the_decision_and_paths_bound_counts_the_stored_object() {
    let with_paths = |bytes: usize| PublishTurn {
        diff_summary: Some(TurnDiff {
            files_changed: 2,
            insertions: 0,
            deletions: 0,
            paths: vec!["a".repeat(bytes - 1), "b".into()],
        }),
        ..turn("done", None)
    };
    // `{"paths":["…","b"]}` takes the paths' own bytes plus 17.
    assert_eq!(
        with_paths(MAX_DECISION_AND_PATHS_BYTES - 17).check(),
        Ok(())
    );
    assert!(with_paths(MAX_DECISION_AND_PATHS_BYTES - 16)
        .check()
        .is_err());
}

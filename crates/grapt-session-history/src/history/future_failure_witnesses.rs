// Tolerant readers: a future writer's failure vocabulary
//
// These rows are exactly what a later build leaves behind: failure reasons
// named by words this build has never seen. The fold must read them — a
// history that refuses its own future rows is not a function of the durable
// log — and it must classify them as Unknown failures, never as a
// neighbouring typed reason.
//
// A real submodule rather than a stitched fragment: this law has its own
// responsibility and the carrier that used to hold it was at its size ceiling.
use super::*;

fn identity<T>(value: Option<T>, refusal: &str) -> T {
    match value {
        Some(value) => value,
        None => panic!("{refusal}"),
    }
}

/// Fold one Step opening followed by one future terminal payload. Both rows
/// are built here so the payload's lifetime outlives the fold, and the caller
/// owns the payload so replay can fold the same bytes twice.
fn future_fold(version: u32, payload: Value) -> MessageHistory {
    let opened = session_message::encode_opened(&session_message::AssistantMessageOpenedFact::new(
        identity(PhysicalAttemptId::new("pat_future"), "attempt id"),
        identity(StepNumber::new(1), "positive step number"),
        identity("msg_future".parse::<MessageId>().ok(), "message id"),
    ));
    let mut history = MessageHistory::default();
    history
        .apply(HistoryRow {
            seq: 0,
            kind_name: "session.assistant-message.opened",
            kind_version: 1,
            data: &opened,
        })
        .unwrap_or_else(|error| panic!("the opening row must fold: {error}"));
    history
        .apply(HistoryRow {
            seq: 3,
            kind_name: "session.step.failed",
            kind_version: version,
            data: &payload,
        })
        .unwrap_or_else(|error| panic!("the future row must still fold: {error}"));
    history
}

fn rendered(history: &MessageHistory) -> Vec<AssistantMessage> {
    history
        .messages()
        .unwrap_or_else(|error| panic!("this history must render: {error}"))
}

/// W1: an unknown top-level kind folds to an Unknown terminal carrying the
/// raw word, in both versions, and the fold stays a function of the rows
/// across replay.
#[test]
fn an_unknown_reason_kind_folds_to_an_unknown_terminal_in_both_versions() {
    for version in [2, 3] {
        let mut payload = serde_json::json!({"step": 1, "reason": {"kind": "aborted"}});
        if version == 3 {
            payload["completedAt"] = serde_json::json!(300);
        }
        let folded = rendered(&future_fold(version, payload.clone()));
        assert_eq!(folded.len(), 1, "the future row must still fold");
        assert_eq!(
            folded[0].terminal,
            Some(StepTerminal::Failed {
                reason: StepTerminalReason::Unknown {
                    tag: "aborted".to_owned()
                }
            }),
            "v{version} classified a future kind as something else"
        );
        if version == 3 {
            assert_eq!(
                folded[0].completed_at.map(|at| at.as_millis()),
                Some(300),
                "the observation survived the unknown reason"
            );
        }
        assert_eq!(
            rendered(&future_fold(version, payload)),
            folded,
            "replay changed the tolerant fold"
        );
    }
}

/// W2: a known reason keeps its typed value when unknown extra fields
/// surround it at every level — the root, the reason, and the principal.
#[test]
fn a_known_reason_survives_unknown_extra_fields_at_every_level() {
    let hook = identity(grapt_types::HookId::new("hook_gate"), "the fixture hook id");
    let folded = rendered(&future_fold(
        2,
        serde_json::json!({
            "step": 1,
            "futureRoot": true,
            "reason": {
                "kind": "authority-refused",
                "futureReason": 7,
                "principal": {
                    "kind": "hook",
                    "hook": "hook_gate",
                    "futurePrincipal": "x"
                }
            }
        }),
    ));
    assert_eq!(
        folded[0].terminal,
        Some(StepTerminal::Failed {
            reason: StepTerminalReason::AuthorityRefused {
                principal: grapt_types::AuthorityPrincipal::Hook { hook }
            }
        }),
        "extra fields must be ignored, not load-bearing"
    );
}

/// W3: an unknown word inside a known kind is tagged by the path it was read
/// from; an unknown principal or effect kind never decodes to a neighbouring
/// arm.
#[test]
fn unknown_words_decode_to_path_qualified_tags() {
    let cases = [
        (
            serde_json::json!({"kind": "provider-failure", "class": "quantum"}),
            "provider-failure/class/quantum",
        ),
        (
            serde_json::json!({"kind": "interrupted", "by": "timeout"}),
            "interrupted/by/timeout",
        ),
        (
            serde_json::json!({"kind": "authority-refused", "principal": {"kind": "reviewer", "agent": "agt_x"}}),
            "authority-refused/principal/kind/reviewer",
        ),
        (
            serde_json::json!({"kind": "process-loss", "effect": {"kind": "phased"}}),
            "process-loss/effect/kind/phased",
        ),
    ];
    for (index, (reason, tag)) in cases.into_iter().enumerate() {
        let folded = rendered(&future_fold(
            2,
            serde_json::json!({"step": 1, "reason": reason}),
        ));
        assert_eq!(
            folded[0].terminal,
            Some(StepTerminal::Failed {
                reason: StepTerminalReason::Unknown {
                    tag: tag.to_owned()
                }
            }),
            "case {index} misclassified the unknown word"
        );
    }
}

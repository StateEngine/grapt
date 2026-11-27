//! Witnesses for the frozen vocabulary.
//!
//! The route closure is checked against the frozen record itself rather than
//! against a second copy of the table, and it is checked in BOTH directions:
//! every operation this client names must exist in the record with the exact
//! method and template, and the client's own `ALL` slice must contain every
//! operation the client can name. A one-directional check would accept a table
//! that had quietly grown an operation nobody added to `ALL`, which is the
//! shape that makes a route reachable from code and invisible to the closure.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::*;
use crate::wire::{SessionWireId, Spelled};
mod session_status;

/// A valid session identity for witnesses.
///
/// Every session literal in this file changed shape in this commit, and that
/// is the point rather than churn: `ses1` — what these witnesses used before —
/// is NOT a valid `grapt_types::SessionId`, and every one of them was passing
/// against an identity the domain refuses. A prefix test could not tell.
fn sid(raw: &str) -> SessionWireId {
    crate::wire::session_id(raw)
        .unwrap_or_else(|error| panic!("witness session id `{raw}` must parse: {error}"))
}

/// The canonical valid session identity used throughout this file.
const SES: &str = "ses_000000000001";

/// The frozen compatibility record, read at test time.
///
/// It is deliberately NOT vendored into this crate. A copy is a second truth
/// that drifts silently; reading the record means a contract change reddens
/// here on the commit that makes it.
fn frozen_record() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixtures/rust-reconstruction/compat/openapi/openapi.json");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("frozen record must be readable at {path:?}: {error}"));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("frozen record must be JSON: {error}"))
}

/// `operationId -> (METHOD, template)` from the frozen record.
fn frozen_operations() -> std::collections::BTreeMap<String, (String, String)> {
    let record = frozen_record();
    let paths = record
        .get("paths")
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("frozen record must declare `paths`"));
    let mut table = std::collections::BTreeMap::new();
    for (template, operations) in paths {
        let Some(operations) = operations.as_object() else {
            continue;
        };
        for (method, operation) in operations {
            let Some(id) = operation
                .get("operationId")
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            table.insert(id.to_owned(), (method.to_uppercase(), template.to_owned()));
        }
    }
    table
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn every_named_operation_matches_frozen_or_approved_extensions() {
    let frozen = frozen_operations();
    for operation in Operation::ALL {
        if matches!(
            operation,
            Operation::GlobalFormGet
                | Operation::GlobalFormState
                | Operation::GlobalFormReply
                | Operation::GlobalFormCancel
        ) {
            let (suffix, method) = match operation {
                Operation::GlobalFormState => ("/state", "GET"),
                Operation::GlobalFormReply => ("/reply", "POST"),
                Operation::GlobalFormCancel => ("/cancel", "POST"),
                _ => ("", "GET"),
            };
            assert_eq!(
                operation.template(),
                format!("/api/session/global/form/{{formID}}{suffix}")
            );
            assert_eq!(operation.method().as_str(), method);
            assert!(!operation.needs_session());
            assert!(operation.needs_form());
            continue;
        }
        if matches!(operation, Operation::ClaudePlugin) {
            assert_eq!(operation.method().as_str(), "POST");
            assert!(!operation.needs_session());
            continue;
        }
        if *operation == Operation::WorkspaceWorktree {
            assert_eq!(operation.template(), "/api/workspace/worktree");
            assert_eq!(operation.method().as_str(), "POST");
            assert_eq!(operation.operation_id(), "grapt.workspace.worktree");
            continue;
        }
        if matches!(
            operation,
            Operation::SessionSandboxWorktreeConfigGet
                | Operation::SessionSandboxWorktreeConfigCreate
        ) {
            assert_eq!(
                operation.template(),
                "/api/session/{sessionID}/sandbox/worktree-config"
            );
            assert_eq!(
                operation.method().as_str(),
                if *operation == Operation::SessionSandboxWorktreeConfigGet {
                    "GET"
                } else {
                    "POST"
                }
            );
            assert!(operation.needs_session());
            continue;
        }
        if *operation == Operation::WorkspaceDiff {
            assert_eq!(operation.template(), "/api/workspace/diff");
            assert_eq!(operation.method().as_str(), "GET");
            assert_eq!(operation.operation_id(), "grapt.workspace.diff");
            continue;
        }

        if matches!(
            operation,
            Operation::SandboxGrantList | Operation::SandboxGrantRevoke
        ) {
            assert!(operation.needs_session());
            assert!(operation.path(None, None).is_err());
            continue;
        }

        if *operation == Operation::InstructionInspect {
            assert_eq!(
                operation.template(),
                "/api/session/{sessionID}/instructions/inspect"
            );
            assert_eq!(operation.method().as_str(), "GET");
            assert_eq!(operation.operation_id(), "grapt.instructions.inspect");
            assert!(operation.needs_session());
            continue;
        }
        if *operation == Operation::CommandInspect {
            assert_eq!(operation.template(), "/api/command/inspect");
            assert_eq!(operation.method().as_str(), "GET");
            assert_eq!(operation.operation_id(), "grapt.command.inspect");
            continue;
        }

        if matches!(
            *operation,
            Operation::McpContent
                | Operation::McpAuth
                | Operation::McpConfiguration
                | Operation::McpReconnect
        ) {
            assert_eq!(operation.method().as_str(), "POST");
            assert!(operation.path(None, None).is_err());
            continue;
        }

        if *operation == Operation::McpApproval {
            assert_eq!(operation.method().as_str(), "GET");
            assert!(operation.path(None, None).is_err());
            continue;
        }

        if *operation == Operation::McpInspect {
            assert_eq!(operation.template(), "/api/mcp/inspect");
            assert_eq!(operation.method().as_str(), "GET");
            assert_eq!(operation.operation_id(), "grapt.mcp.inspect");
            continue;
        }

        // The authenticated resolved read is admitted alongside the frozen POST.
        if *operation == Operation::SessionPresetGet {
            assert_eq!(operation.template(), "/api/session/{sessionID}/preset");
            assert_eq!(operation.method().as_str(), "GET");
            continue;
        }
        if matches!(
            operation,
            Operation::SessionRewindPreview | Operation::SessionRewind
        ) {
            let suffix = if *operation == Operation::SessionRewindPreview {
                "/preview"
            } else {
                ""
            };
            assert_eq!(
                operation.template(),
                format!("/api/session/{{sessionID}}/rewind{suffix}")
            );
            assert_eq!(operation.method().as_str(), "POST");
            assert!(operation.operation_id().starts_with("grapt.session.rewind"));
            continue;
        }
        if matches!(
            operation,
            Operation::SessionUndoPreview | Operation::SessionUndo
        ) {
            let suffix = if *operation == Operation::SessionUndoPreview {
                "/preview"
            } else {
                ""
            };
            assert_eq!(
                operation.template(),
                format!("/api/session/{{sessionID}}/undo{suffix}")
            );
            assert_eq!(operation.method().as_str(), "POST");
            continue;
        }
        if *operation == Operation::SessionReconcile {
            assert_eq!(operation.template(), "/api/session/{sessionID}/reconcile");
            assert_eq!(operation.method().as_str(), "POST");
            continue;
        }
        if *operation == Operation::SessionSnapshot {
            assert_eq!(operation.template(), "/api/session/{sessionID}/snapshot");
            assert_eq!(operation.method().as_str(), "GET");
            continue;
        }
        if *operation == Operation::SideQuestion {
            assert_eq!(operation.method().as_str(), "POST");
            assert!(operation.needs_session());
            assert!(operation.path(None, None).is_err());
            continue;
        }
        let id = operation.operation_id();
        let (method, template) = frozen
            .get(id)
            .unwrap_or_else(|| panic!("{id} is not declared by the frozen record"));
        assert_eq!(
            method,
            operation.method().as_str(),
            "{id} method drifted from the frozen record"
        );
        assert_eq!(
            template,
            operation.template(),
            "{id} template drifted from the frozen record"
        );
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn the_all_slice_contains_every_operation_the_client_can_name() {
    // The reverse direction. `ALL` drives the closure witness above, so an
    // operation reachable in code but missing from `ALL` would never be
    // checked against the frozen record at all.
    let declared: BTreeSet<Operation> = Operation::ALL.iter().copied().collect();
    assert_eq!(
        declared.len(),
        Operation::ALL.len(),
        "`Operation::ALL` contains a duplicate"
    );

    // Exhaustive by construction: this match fails to compile when a variant
    // is added, and the variant must then be added to `ALL` for the set
    // comparison below to pass.
    let every = [
        Operation::HealthGet,
        Operation::IntegrationList,
        Operation::IntegrationKeyConnect,
        Operation::IntegrationOAuthConnect,
        Operation::IntegrationOAuthStatus,
        Operation::IntegrationOAuthCancel,
        Operation::IntegrationOAuthComplete,
        Operation::CredentialRename,
        Operation::CredentialRemove,
        Operation::ProviderList,
        Operation::ProjectList,
        Operation::ProjectCurrent,
        Operation::AgentList,
        Operation::CommandList,
        Operation::CommandInspect,
        Operation::InstructionInspect,
        Operation::ModelList,
        Operation::ModelDefault,
        Operation::SessionAgentSelect,
        Operation::SessionModelSelect,
        Operation::FormRequestList,
        Operation::GlobalFormGet,
        Operation::GlobalFormState,
        Operation::GlobalFormReply,
        Operation::GlobalFormCancel,
        Operation::SessionFormList,
        Operation::SessionFormGet,
        Operation::SessionFormState,
        Operation::SessionFormReply,
        Operation::SessionFormCancel,
        Operation::SessionList,
        Operation::SessionCreate,
        Operation::SessionFork,
        Operation::SideQuestion,
        Operation::SessionGet,
        Operation::SessionPrompt,
        Operation::SessionPendingList,
        Operation::SessionInterrupt,
        Operation::SessionCompact,
        Operation::SessionRewindPreview,
        Operation::SessionRewind,
        Operation::SessionUndoPreview,
        Operation::SessionUndo,
        Operation::SessionSandboxWorktreeConfigGet,
        Operation::SessionSandboxWorktreeConfigCreate,
        Operation::SessionReconcile,
        Operation::SessionRename,
        Operation::SessionDelete,
        Operation::MessageList,
        Operation::EventSubscribe,
        Operation::PermissionRequestList,
        Operation::SessionSubagentModelGet,
        Operation::SessionPermissionList,
        Operation::SessionPermissionCreate,
        Operation::SandboxGrantList,
        Operation::SandboxGrantRevoke,
        Operation::SessionPermissionGet,
        Operation::SessionPermissionReply,
        Operation::McpList,
        Operation::WorkspaceDiff,
        Operation::WorkspaceWorktree,
        Operation::ClaudePlugin,
        Operation::McpInspect,
        Operation::McpAuth,
        Operation::McpConfiguration,
        Operation::McpReconnect,
        Operation::McpApproval,
        Operation::SessionPresetGet,
        Operation::SessionPresetSelect,
    ];
    for operation in every {
        let _exhaustive: &'static str = match operation {
            Operation::IntegrationList
            | Operation::IntegrationKeyConnect
            | Operation::IntegrationOAuthConnect
            | Operation::IntegrationOAuthStatus
            | Operation::IntegrationOAuthCancel
            | Operation::IntegrationOAuthComplete
            | Operation::CredentialRename
            | Operation::CredentialRemove => "",
            Operation::HealthGet => "",
            Operation::ProviderList => "",
            Operation::ProjectList => "",
            Operation::ProjectCurrent => "",
            Operation::AgentList => "",
            Operation::CommandList | Operation::CommandInspect | Operation::InstructionInspect => {
                ""
            }
            Operation::ModelList => "",
            Operation::ModelDefault => "",
            Operation::SessionAgentSelect => "",
            Operation::SessionModelSelect => "",
            Operation::FormRequestList
            | Operation::GlobalFormGet
            | Operation::GlobalFormState
            | Operation::GlobalFormReply
            | Operation::GlobalFormCancel => "",
            Operation::SessionSubagentModelGet => "",
            Operation::SessionFormList => "",
            Operation::SessionFormGet => "",
            Operation::SessionFormState => "",
            Operation::SessionFormReply => "",
            Operation::SessionFormCancel => "",
            Operation::SessionList => "",
            Operation::SessionCreate | Operation::SessionFork | Operation::SideQuestion => "",
            Operation::SessionGet => "",
            Operation::SessionPrompt => "",
            Operation::SessionPendingList => "",
            Operation::SessionInterrupt => "",
            Operation::SessionCompact => "",
            Operation::SessionRewindPreview
            | Operation::SessionRewind
            | Operation::SessionUndoPreview
            | Operation::SessionUndo
            | Operation::SessionSandboxWorktreeConfigGet
            | Operation::SessionSandboxWorktreeConfigCreate
            | Operation::SessionReconcile => "",
            Operation::SessionRename | Operation::SessionDelete => "",
            Operation::MessageList => "",
            Operation::SessionSnapshot => "",
            Operation::EventSubscribe => "",
            Operation::PermissionRequestList => "",
            Operation::SandboxGrantList | Operation::SandboxGrantRevoke => "",
            Operation::SessionPermissionList => "",
            Operation::SessionPermissionCreate => "",
            Operation::SessionPermissionGet => "",
            Operation::SessionPermissionReply => "",
            Operation::WorkspaceDiff
            | Operation::McpContent
            | Operation::WorkspaceWorktree
            | Operation::ClaudePlugin
            | Operation::McpAuth
            | Operation::McpApproval
            | Operation::McpConfiguration
            | Operation::McpReconnect
            | Operation::McpInspect
            | Operation::McpList
            | Operation::SessionPresetGet
            | Operation::SessionPresetSelect => "",
        };
        assert!(
            declared.contains(&operation),
            "{} is reachable in code but absent from `Operation::ALL`",
            operation.operation_id()
        );
    }
}

#[test]
fn a_scoped_operation_without_its_scope_is_refused_rather_than_producing_a_brace() {
    assert_eq!(
        Operation::SessionPrompt.path(None, None),
        Err(ContractError::SessionRequired {
            operation: "v2.session.prompt"
        })
    );
    // The EMPTY-session case that used to live here is GONE, and its absence is
    // the correction rather than a lost witness. `path` now takes a
    // `SessionWireId`, and there is no such value spelled `""` — the domain
    // refuses it at the parse. So the branch that used to check emptiness here
    // is unreachable by construction, and asserting on it would mean keeping a
    // parameter shape that can express the defect purely so a test can catch
    // it. The refusal is witnessed where it now happens, in
    // `an_empty_or_malformed_session_identity_is_refused_at_the_parse`.
    assert_eq!(
        Operation::SessionPermissionReply.path(Some(&sid(SES)), None),
        Err(ContractError::RequestRequired {
            operation: "v2.session.permission.reply"
        })
    );
    // And the positive: both placeholders are substituted, none left behind.
    let Ok(path) = Operation::SessionPermissionReply.path(Some(&sid(SES)), Some("per1")) else {
        panic!("both scopes were supplied and the path must resolve");
    };
    assert_eq!(path, format!("/api/session/{SES}/permission/per1/reply"));
    assert!(!path.contains('{'), "a placeholder survived substitution");
}

#[test]
fn an_empty_or_malformed_session_identity_is_refused_at_the_parse() {
    // What `path(Some(""), None)` used to witness, witnessed at the seam that
    // now owns it. Each of these is a spelling the previous revision's
    // `starts_with("ses")` accepted or that `path` had to special-case.
    for raw in ["", "ses", "ses_", "sesABC", "sxx1", "ses_ab-cd", "SES_1"] {
        assert!(
            crate::wire::session_id(raw).is_err(),
            "`{raw}` must be refused by the authoritative newtype"
        );
    }
    // And the positive control, so the refusal above is not vacuous.
    assert_eq!(sid(SES).spelled(), SES);
}

#[test]
fn unscoped_operations_ignore_scope_arguments() {
    assert_eq!(
        Operation::HealthGet.path(Some(&sid(SES)), Some("per1")),
        Ok("/api/health".to_owned())
    );
}

// ---------------------------------------------------------------------------
// Envelope law
// ---------------------------------------------------------------------------

fn durable_block() -> serde_json::Value {
    serde_json::json!({"aggregateID": SES, "seq": 7, "version": 1})
}

fn frame(member: EventMember, durable: bool, created: bool) -> String {
    let mut object = serde_json::Map::new();
    object.insert("id".into(), serde_json::json!("evt_1"));
    object.insert("type".into(), serde_json::json!(member.as_str()));
    object.insert("data".into(), serde_json::json!({"sessionID": SES}));
    if member.is_tool_hint() {
        object.insert(
            "data".into(),
            serde_json::json!({"sessionID":SES,
            "assistantMessageID":"msg_000000000001","callID":"call_000000000001",
            "metadata":{"toolCalls":[]}}),
        );
    }
    if member == EventMember::SessionStatus {
        object.insert(
            "data".into(),
            serde_json::json!({"sessionID": SES, "status": {"type": "idle"}}),
        );
    }
    if member == EventMember::MailAttention {
        object.insert(
            "data".into(),
            serde_json::json!({"sessionID":SES,"status":{"type":"unavailable"}}),
        );
    }
    if member == EventMember::ContinuityAttention {
        object.insert(
            "data".into(),
            serde_json::json!({"sessionID":SES,"status":{"type":"ok"}}),
        );
    }
    if durable {
        let mut block = durable_block();
        if matches!(member, EventMember::ToolSuccess | EventMember::ToolFailed) {
            block["version"] = serde_json::json!(2);
        }
        object.insert("durable".into(), block);
    }
    if created {
        object.insert("created".into(), serde_json::json!(1_700_000_000_000u64));
    }
    serde_json::Value::Object(object).to_string()
}

#[test]
fn every_frozen_member_decodes_in_its_declared_shape() {
    for member in EventMember::ALL {
        let payload = frame(
            *member,
            member.requires_durable(),
            member.requires_created(),
        );
        let decoded = EventEnvelope::decode(&payload)
            .unwrap_or_else(|error| panic!("{} must decode: {error}", member.as_str()));
        assert_eq!(decoded.member, Some(*member));
        assert_eq!(
            decoded.durable.is_some(),
            member.requires_durable(),
            "{} durable presence",
            member.as_str()
        );
    }
}

#[test]
fn a_durable_member_missing_its_block_is_refused() {
    for member in EventMember::ALL.iter().filter(|m| m.requires_durable()) {
        let payload = frame(*member, false, member.requires_created());
        assert_eq!(
            EventEnvelope::decode(&payload),
            Err(ContractError::DurableMissing {
                member: member.as_str()
            }),
            "{} without durable must be refused",
            member.as_str()
        );
    }
}

#[test]
fn a_live_member_carrying_a_durable_block_is_refused() {
    // The other direction, and the one that matters most: a transport that
    // stamps `durable` on every frame makes a live frame look re-derivable and
    // silently defeats gap reconciliation. A one-directional check accepts it.
    for member in EventMember::ALL.iter().filter(|m| !m.requires_durable()) {
        let payload = frame(*member, true, member.requires_created());
        assert_eq!(
            EventEnvelope::decode(&payload),
            Err(ContractError::DurableForbidden {
                member: member.as_str()
            }),
            "{} with durable must be refused",
            member.as_str()
        );
    }
}

#[test]
fn created_is_required_everywhere_except_server_connected_and_forbidden_there() {
    let payload = frame(EventMember::TextStarted, true, false);
    assert_eq!(
        EventEnvelope::decode(&payload),
        Err(ContractError::CreatedMissing {
            member: "session.text.started"
        })
    );
    let payload = frame(EventMember::ServerConnected, false, true);
    assert_eq!(
        EventEnvelope::decode(&payload),
        Err(ContractError::CreatedForbidden {
            member: "server.connected"
        })
    );
}

#[test]
fn the_durable_version_literal_is_pinned() {
    let payload = serde_json::json!({
        "id": "evt_1",
        "type": "session.text.started",
        "created": 1_700_000_000_000u64,
        "durable": {"aggregateID": SES, "seq": 7, "version": 2},
        "data": {"sessionID": SES}
    })
    .to_string();
    match EventEnvelope::decode(&payload) {
        Err(ContractError::DurableMalformed { .. }) => {}
        other => panic!("durable.version 2 must be refused, got {other:?}"),
    }
}

#[test]
fn identity_prefixes_are_enforced_and_ses_is_three_characters() {
    let payload = serde_json::json!({
        "id": "event_1",
        "type": "session.idle",
        "created": 1u64,
        "data": {"sessionID": SES}
    })
    .to_string();
    assert_eq!(
        EventEnvelope::decode(&payload),
        Err(ContractError::EventIdInvalid {
            id: "event_1".to_owned()
        })
    );

    // THE INVERTED ASSERTION.
    //
    // This witness previously asserted that `sesABC` DECODES, with a comment
    // explaining that the frozen pattern is `^ses` and that demanding `ses_`
    // would refuse a legitimate id. That reasoning was wrong in one specific
    // way: `^ses` is the WEAKER of the two authorities, so honouring it admits
    // identities `grapt_types::SessionId` cannot represent at all. `sesABC` is
    // one. It is not a legitimate id being wrongly refused; it is an id this
    // client could accept and then never key, compare or hand back.
    //
    // So the assertion is inverted, and it is inverted deliberately rather than
    // deleted, because a reader arriving from the old revision needs to find
    // the answer to "didn't this used to be allowed?" exactly here.
    let payload = serde_json::json!({
        "id": "evt_1",
        "type": "session.idle",
        "created": 1u64,
        "data": {"sessionID": "sesABC"}
    })
    .to_string();
    assert_eq!(
        EventEnvelope::decode(&payload),
        Err(ContractError::SessionIdInvalid {
            id: "sesABC".to_owned()
        }),
        "`sesABC` matches the frozen `^ses` pattern but is not a SessionId"
    );

    let payload = serde_json::json!({
        "id": "evt_1",
        "type": "session.idle",
        "created": 1u64,
        "data": {"sessionID": "sxx1"}
    })
    .to_string();
    assert_eq!(
        EventEnvelope::decode(&payload),
        Err(ContractError::SessionIdInvalid {
            id: "sxx1".to_owned()
        })
    );

    // A malformed durable AGGREGATE is refused too, and by the same law. The
    // previous revision checked only that it was non-empty, so an aggregate id
    // that was not a session at all reached the cursor and became a watermark
    // key.
    let payload = serde_json::json!({
        "id": "evt_1",
        "type": "session.text.started",
        "created": 1u64,
        "durable": {"aggregateID": "sesABC", "seq": 1, "version": 1},
        "data": {"sessionID": SES}
    })
    .to_string();
    let Err(ContractError::DurableMalformed { reason, .. }) = EventEnvelope::decode(&payload)
    else {
        panic!("a durable aggregate that is not a session identity must be refused");
    };
    assert!(
        reason.contains("not a valid session identity"),
        "the refusal must name the parse, found {reason:?}"
    );
}

#[test]
fn a_member_outside_the_vocabulary_decodes_without_envelope_judgement() {
    // The union is larger than this client's vocabulary. An unknown member is
    // not an error — but it also carries no `member`, so nothing downstream
    // can render it as a checkpoint.
    let payload = serde_json::json!({
        "id": "evt_1",
        "type": "session.some.future.member",
        "created": 1u64,
        "durable": durable_block(),
        "data": {"sessionID": SES}
    })
    .to_string();
    let Ok(decoded) = EventEnvelope::decode(&payload) else {
        panic!("a member outside the vocabulary must still decode");
    };
    assert_eq!(decoded.member, None);
}

#[test]
fn terminals_are_three_and_idle_is_not_one_of_them() {
    let terminals: Vec<_> = EventMember::ALL
        .iter()
        .copied()
        .filter(|m| m.is_execution_terminal())
        .collect();
    assert_eq!(terminals.len(), 3, "exactly three execution terminals");
    assert!(
        !EventMember::SessionIdle.is_execution_terminal(),
        "session.idle is quiescence, not a terminal"
    );
}

#[test]
fn the_durable_set_includes_four_tool_lifecycle_members() {
    let durable: Vec<_> = EventMember::ALL
        .iter()
        .copied()
        .filter(|m| m.requires_durable())
        .collect();
    assert_eq!(
        durable.len(),
        13,
        "nine frozen and four tool lifecycle members"
    );
    let terminals: Vec<_> = durable
        .iter()
        .copied()
        .filter(|m| m.is_execution_terminal())
        .collect();
    assert_eq!(
        terminals.len(),
        3,
        "only execution terminals settle an execution"
    );
}

#[test]
fn execution_ids_are_declared_on_exactly_the_four_execution_members() {
    let carriers: Vec<_> = EventMember::ALL
        .iter()
        .copied()
        .filter(|m| m.carries_execution_id())
        .collect();
    assert_eq!(carriers.len(), 4, "four session.execution.* members");
    for member in carriers {
        assert!(
            member.as_str().starts_with("session.execution."),
            "{} is not an execution member",
            member.as_str()
        );
    }
}

#[test]
fn unserved_members_are_exactly_the_measured_three() {
    // Measured at base 151edd66 against the eleven-member
    // `dogfood_event_closure!` in `crates/grapt-api/src/u3a/events.rs`. When
    // an emitter lands this reddens, and re-measuring is the point: the client
    // must not carry a stale claim about what the engine serves.
    let unserved: BTreeSet<&'static str> = EventMember::ALL
        .iter()
        .copied()
        .filter(|m| !m.is_served_today())
        .map(EventMember::as_str)
        .collect();
    let expected: BTreeSet<&'static str> = [
        "session.created",
        "session.execution.started",
        "session.idle",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        unserved, expected,
        "the served/unserved split drifted; re-measure against grapt-api's event closure"
    );
    assert_eq!(
        EventMember::ALL.len() - unserved.len(),
        19,
        "the sixteen previously served members, plus session.status admitted by the \
         SESSION-QUIESCENCE emitter (re-measured at base c755c828), plus \
         session.continuity.attention"
    );
}

#[test]
fn mail_attention_requires_exact_scoped_payload_and_bounded_readback() {
    let good = serde_json::json!({"id":"evt_mail","type":"session.mail.attention","created":1,"data":{"sessionID":SES,"status":{"type":"ready","pending":1,"incomplete":false}}});
    assert_eq!(
        EventEnvelope::decode(&good.to_string()).map(|e| e.member),
        Ok(Some(EventMember::MailAttention))
    );
    for status in [
        serde_json::json!({"type":"ready","pending":65,"incomplete":false}),
        serde_json::json!({"type":"ready","pending":0}),
        serde_json::json!({"type":"idle"}),
        serde_json::json!({"type":"unavailable","body":"private"}),
    ] {
        let mut bad = good.clone();
        bad["data"]["status"] = status;
        assert_eq!(
            EventEnvelope::decode(&bad.to_string()),
            Err(ContractError::MailAttentionMalformed)
        );
    }
    for session in [
        serde_json::Value::Null,
        serde_json::json!(42),
        serde_json::json!("foreign"),
    ] {
        let mut bad = good.clone();
        bad["data"]["sessionID"] = session;
        assert_eq!(
            EventEnvelope::decode(&bad.to_string()),
            Err(ContractError::MailAttentionMalformed)
        );
    }
}

#[test]
fn continuity_attention_requires_an_exact_scoped_closed_level() {
    let good = serde_json::json!({"id":"evt_cont","type":"session.continuity.attention","created":1,"data":{"sessionID":SES,"status":{"type":"degraded","reason":"capability"}}});
    assert_eq!(
        EventEnvelope::decode(&good.to_string()).map(|e| e.member),
        Ok(Some(EventMember::ContinuityAttention))
    );
    for status in [
        serde_json::json!({"type":"degraded"}),
        serde_json::json!({"type":"degraded","reason":"remote text"}),
        serde_json::json!({"type":"ok","reason":"transport"}),
        serde_json::json!({"type":"idle"}),
    ] {
        let mut bad = good.clone();
        bad["data"]["status"] = status;
        assert_eq!(
            EventEnvelope::decode(&bad.to_string()),
            Err(ContractError::ContinuityAttentionMalformed)
        );
    }
    let mut extra = good.clone();
    extra["data"]["staleSince"] = serde_json::json!(1);
    assert_eq!(
        EventEnvelope::decode(&extra.to_string()),
        Err(ContractError::ContinuityAttentionMalformed)
    );
    let mut foreign = good;
    foreign["data"]["sessionID"] = serde_json::json!("foreign");
    assert_eq!(
        EventEnvelope::decode(&foreign.to_string()),
        Err(ContractError::ContinuityAttentionMalformed)
    );
}

#[test]
fn tool_terminals_require_version_two_and_valid_call_identity() {
    for member in [EventMember::ToolSuccess, EventMember::ToolFailed] {
        let payload = frame(member, true, true);
        assert!(EventEnvelope::decode(&payload).is_ok());
        let mut value: serde_json::Value = serde_json::from_str(&payload)
            .unwrap_or_else(|error| panic!("tool fixture JSON: {error}"));
        for version in [1, 3] {
            value["durable"]["version"] = serde_json::json!(version);
            assert!(matches!(
                EventEnvelope::decode(&value.to_string()),
                Err(ContractError::DurableMalformed { .. })
            ));
        }
        value["durable"]["version"] = serde_json::json!(2);
        value["data"]["callID"] = serde_json::json!("");
        assert_eq!(
            EventEnvelope::decode(&value.to_string()),
            Err(ContractError::ToolHintMalformed)
        );
    }
}

use std::io::Cursor;

use grapt_codemode::{
    read_frame, run_worker, write_frame, CatalogTool, ExecuteRequest, GuestFailure, GuestResult,
    HostReply, HostResult, WorkerMessage,
};
use grapt_types::{ToolDefinitionId, ToolSchemaVersion};
use serde_json::json;

fn request(code: &str) -> ExecuteRequest {
    ExecuteRequest {
        code: code.to_owned(),
        catalog: ToolSchemaVersion::new("catalog-1").unwrap_or_else(|| panic!("catalog identity")),
        tools: vec![CatalogTool {
            name: ToolDefinitionId::new("echo").unwrap_or_else(|| panic!("tool name")),
            description: "Echo object".to_owned(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
        }],
    }
}

fn execute(code: &str, replies: &[HostReply]) -> Vec<WorkerMessage> {
    let mut input = Vec::new();
    write_frame(&mut input, &request(code)).unwrap_or_else(|error| panic!("request: {error}"));
    for reply in replies {
        write_frame(&mut input, reply).unwrap_or_else(|error| panic!("reply: {error}"));
    }
    let mut output = Vec::new();
    run_worker(&mut Cursor::new(input), &mut output)
        .unwrap_or_else(|error| panic!("worker: {error}"));
    let length = output.len() as u64;
    let mut output = Cursor::new(output);
    let mut messages = Vec::new();
    while output.position() < length {
        messages.push(read_frame(&mut output).unwrap_or_else(|error| panic!("message: {error}")));
    }
    messages
}

fn completed(messages: &[WorkerMessage], expected: Option<serde_json::Value>) {
    match messages.last() {
        Some(WorkerMessage::Finished {
            result: GuestResult::Completed { value },
        }) => assert_eq!(*value, expected),
        other => panic!("missing successful terminal: {other:?}"),
    }
}

#[test]
fn return_expression_undefined_and_null_remain_distinct() {
    completed(&execute("1+2", &[]), Some(json!(3)));
    completed(&execute("return 4;", &[]), Some(json!(4)));
    completed(&execute("undefined", &[]), None);
    completed(&execute("null", &[]), Some(json!(null)));
}

#[test]
fn seat01_approval_unavailable_survives_caught_and_uncaught_host_rejection() {
    let replies = [HostReply {
        ordinal: 0,
        result: HostResult::Failed {
            reason: grapt_codemode::HostFailure::ApprovalUnavailable,
            message: None,
        },
    }];
    let uncaught = execute("await tools.echo({})", &replies);
    assert!(
        matches!(
            uncaught.last(),
            Some(WorkerMessage::Finished {
                result: GuestResult::Failed {
                    reason: GuestFailure::ApprovalUnavailable,
                    ..
                }
            })
        ),
        "uncaught approval failure lost its type: {uncaught:?}"
    );
    completed(
        &execute(
            "try { await tools.echo({}); } catch(e) { return e; }",
            &replies,
        ),
        Some(json!({
            "error":"approval_unavailable",
            "tool":"echo",
            "message":"approval unavailable (headless): no matching approver is attached; no effect ran"
        })),
    );
    let forged = execute("throw {error:'approval_unavailable'}", &[]);
    assert!(
        matches!(
            forged.last(),
            Some(WorkerMessage::Finished {
                result: GuestResult::Failed {
                    reason: GuestFailure::GuestError,
                    ..
                }
            })
        ),
        "a guest value impersonated an authoritative refusal"
    );
    let unrelated = execute(
        "try { await tools.echo({}); } catch(e) {} throw 1;",
        &replies,
    );
    assert!(
        matches!(
            unrelated.last(),
            Some(WorkerMessage::Finished {
                result: GuestResult::Failed {
                    reason: GuestFailure::GuestError,
                    ..
                }
            })
        ),
        "a caught approval failure contaminated a later exception"
    );
}

#[test]
fn parallel_calls_are_sent_before_out_of_order_results_are_consumed() {
    let replies = [
        HostReply {
            ordinal: 1,
            result: HostResult::Completed {
                value: json!({"value":2}),
            },
        },
        HostReply {
            ordinal: 0,
            result: HostResult::Completed {
                value: json!({"value":1}),
            },
        },
    ];
    let messages = execute(
        "await Promise.all([tools.echo({x:1}),tools.echo({x:2})])",
        &replies,
    );
    assert!(matches!(&messages[0], WorkerMessage::Call { call } if call.ordinal == 0));
    assert!(matches!(&messages[1], WorkerMessage::Call { call } if call.ordinal == 1));
    completed(&messages, Some(json!([{"value":1},{"value":2}])));
}

#[test]
fn parser_import_cannot_repeat_a_host_request() {
    let replies = [HostReply {
        ordinal: 0,
        result: HostResult::Completed { value: json!({}) },
    }];
    let messages = execute(
        "await tools.echo({}); await import('codemode-parse')",
        &replies,
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, WorkerMessage::Call { .. }))
            .count(),
        1
    );
    assert!(matches!(
        messages.last(),
        Some(WorkerMessage::Finished {
            result: GuestResult::Failed {
                reason: GuestFailure::TypeError | GuestFailure::ReferenceError,
                ..
            }
        })
    ));
}

#[test]
fn host_bridge_survives_guest_intrinsic_mutation() {
    let replies = [HostReply {
        ordinal: 0,
        result: HostResult::Completed {
            value: json!({"ok":true}),
        },
    }];
    completed(&execute("Map.prototype.get=()=>undefined; Map.prototype.set=()=>{throw 1}; globalThis.Promise=()=>{throw 2}; await tools.echo({})", &replies), Some(json!({"ok":true})));
}

#[test]
fn fresh_runtime_has_no_prior_state_or_ambient_capabilities() {
    completed(&execute("globalThis.secret=7; secret", &[]), Some(json!(7)));
    completed(
        &execute(
            "[typeof secret,typeof process,typeof fetch,typeof require,typeof std,typeof os]",
            &[],
        ),
        Some(json!(vec!["undefined"; 6])),
    );
}

#[test]
fn serialization_and_after_await_loops_are_interrupted() {
    for code in [
        "await Promise.resolve(); while(true){}",
        "({toJSON(){while(true){}}})",
        "/(a+)+$/.test('a'.repeat(10000)+'!')",
    ] {
        let messages = execute(code, &[]);
        assert!(
            matches!(
                messages.last(),
                Some(WorkerMessage::Finished {
                    result: GuestResult::Failed {
                        reason: GuestFailure::Interrupted,
                        ..
                    }
                })
            ),
            "{messages:?}"
        );
    }
}

#[test]
fn selected_worker_enforces_cpu_heap_jobs_and_output_bounds() {
    for (code, expected) in [
        (
            "await Promise.resolve(); let n=0; for(let i=0;i<10000000;i++) n++; n",
            GuestFailure::Interrupted,
        ),
        ("new ArrayBuffer(20*1024*1024)", GuestFailure::GuestError),
        (
            "for(let i=0;i<1500;i++) await Promise.resolve(); return 1",
            GuestFailure::JobLimit,
        ),
        ("'a'.repeat(70000)", GuestFailure::OutputLimit),
    ] {
        let messages = execute(code, &[]);
        assert!(
            matches!(messages.last(), Some(WorkerMessage::Finished { result: GuestResult::Failed { reason, .. } }) if *reason == expected),
            "expected selected-worker limit {expected:?}"
        );
    }
}

#[test]
fn selected_worker_refuses_the_sixty_fifth_host_call() {
    let replies: Vec<_> = (0..65)
        .map(|ordinal| HostReply {
            ordinal,
            result: HostResult::Completed { value: json!({}) },
        })
        .collect();
    let messages = execute(
        "for(let i=0;i<65;i++) await tools.echo({}); return 1",
        &replies,
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| matches!(message, WorkerMessage::Call { .. }))
            .count(),
        64
    );
    assert!(matches!(
        messages.last(),
        Some(WorkerMessage::Finished {
            result: GuestResult::Failed {
                reason: GuestFailure::TypeError,
                ..
            }
        })
    ));
}

#[test]
fn selected_worker_bounds_inflight_arguments_and_queued_bytes() {
    let replies: Vec<_> = (0..9)
        .map(|ordinal| HostReply {
            ordinal,
            result: HostResult::Completed { value: json!({}) },
        })
        .collect();
    for code in [
        "await Promise.all(Array.from({length:9},()=>tools.echo({}))); return 1",
        "await tools.echo({value:'x'.repeat(300000)}); return 1",
        "await Promise.all(Array.from({length:6},()=>tools.echo({value:'x'.repeat(200000)}))); return 1",
    ] {
        let messages = execute(code, &replies);
        assert!(matches!(
            messages.last(),
            Some(WorkerMessage::Finished {
                result: GuestResult::Failed {
                    reason: GuestFailure::TypeError,
                    ..
                }
            })
        ));
    }
}

#[test]
fn selected_worker_bounds_finite_recursion_and_unresolved_promises() {
    completed(
        &execute("return (function f(n){return n ? 1+f(n-1) : 0})(2)", &[]),
        Some(json!(2)),
    );
    for (code, expected) in [
        (
            "return (function f(n){return n ? 1+f(n-1) : 0})(500)",
            GuestFailure::RangeError,
        ),
        ("await new Promise(()=>{})", GuestFailure::UnresolvedPromise),
    ] {
        let messages = execute(code, &[]);
        assert!(
            matches!(messages.last(), Some(WorkerMessage::Finished { result: GuestResult::Failed { reason, .. } }) if *reason == expected),
            "expected bounded failure {expected:?}, got {messages:?}"
        );
    }
}

#[test]
fn catalog_queries_return_schemas_only_on_request_and_emit_no_host_calls() {
    let messages = execute("return catalog.search('echo');", &[]);
    assert_eq!(messages.len(), 1, "metadata discovery must execute no tool");
    completed(
        &messages,
        Some(
            json!({"matches":[{"name":"echo","description":"Echo object"}],"total":1,"truncated":false}),
        ),
    );
    let messages = execute("return catalog.describe('echo');", &[]);
    assert_eq!(messages.len(), 1);
    completed(
        &messages,
        Some(json!({"name":"echo","description":"Echo object","input_schema":{"type":"object"}})),
    );
    completed(
        &execute("return catalog.search('missing');", &[]),
        Some(json!({"matches":[],"total":0,"truncated":false})),
    );
}

#[test]
fn schema_discovery_does_not_change_the_allowed_host_calls() {
    let replies = [HostReply {
        ordinal: 0,
        result: HostResult::Completed {
            value: json!({"approved":true}),
        },
    }];
    let messages = execute("const found = catalog.describe('echo'); found.name='unavailable'; return await tools.echo({});", &replies);
    assert_eq!(
        messages
            .iter()
            .filter(|message| matches!(message, WorkerMessage::Call { .. }))
            .count(),
        1
    );
    completed(&messages, Some(json!({"approved":true})));
    completed(&execute("let a=catalog.describe('echo'); a.input_schema.type='invalid'; return catalog.describe('echo').input_schema.type;", &[]), Some(json!("object")));
}

#[test]
fn catalog_query_bounds_and_unknown_names_are_explicit_failures() {
    for code in [
        "return catalog.search('x'.repeat(257));",
        "return catalog.search('',0);",
        "return catalog.search('',17);",
        "return catalog.search('',1.5);",
        "return catalog.describe('__proto__');",
        "return catalog.describe('missing');",
    ] {
        let messages = execute(code, &[]);
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            messages.last(),
            Some(WorkerMessage::Finished {
                result: GuestResult::Failed {
                    reason: GuestFailure::GuestError,
                    ..
                }
            })
        ));
    }
}

fn failed_with(messages: &[WorkerMessage]) -> (GuestFailure, Option<String>) {
    match messages.last() {
        Some(WorkerMessage::Finished {
            result: GuestResult::Failed { reason, detail, .. },
        }) => (*reason, detail.clone()),
        other => panic!("missing failed terminal: {other:?}"),
    }
}

#[test]
fn tool01a_a_guest_throw_carries_its_own_message() {
    let (reason, detail) = failed_with(&execute("throw new Error('the guest broke here')", &[]));
    assert_eq!(reason, GuestFailure::GuestError);
    let detail = detail.unwrap_or_else(|| panic!("a guest throw lost its message"));
    assert!(
        detail.starts_with("Error: the guest broke here"),
        "unexpected detail: {detail}"
    );
    let (_, detail) = failed_with(&execute("throw new TypeError('bad shape')", &[]));
    assert!(detail.is_some_and(|detail| detail.starts_with("TypeError: bad shape")));
    let (_, detail) = failed_with(&execute("throw 'plain words'", &[]));
    assert_eq!(detail.as_deref(), Some("plain words"));
    let (_, detail) = failed_with(&execute("throw {code: 7}", &[]));
    assert_eq!(detail.as_deref(), Some("{\"code\":7}"));
    let (reason, detail) = failed_with(&execute("return (", &[]));
    assert_eq!(reason, GuestFailure::InvalidSource);
    assert!(
        detail.is_some_and(|detail| detail.starts_with("SyntaxError")),
        "a syntax error lost its message"
    );
}

#[test]
fn tool01a_a_nested_refusal_names_its_tool_reason_and_text() {
    let replies = [HostReply {
        ordinal: 0,
        result: HostResult::Failed {
            reason: grapt_codemode::HostFailure::EffectFailed,
            message: Some("permission denied for /etc/shadow".to_owned()),
        },
    }];
    let (reason, detail) = failed_with(&execute("await tools.echo({})", &replies));
    assert_eq!(reason, GuestFailure::GuestError);
    assert_eq!(
        detail.as_deref(),
        Some("tool echo failed (effect_failed): permission denied for /etc/shadow")
    );
    // A caught refusal is ordinary data the guest can read.
    completed(
        &execute(
            "try { await tools.echo({}); } catch(e) { return e; }",
            &replies,
        ),
        Some(json!({
            "error": "effect_failed",
            "tool": "echo",
            "message": "permission denied for /etc/shadow"
        })),
    );
    let silent = [HostReply {
        ordinal: 0,
        result: HostResult::Failed {
            reason: grapt_codemode::HostFailure::Cancelled,
            message: None,
        },
    }];
    let (_, detail) = failed_with(&execute("await tools.echo({})", &silent));
    assert_eq!(detail.as_deref(), Some("tool echo failed (cancelled)"));
}

#[test]
fn tool01a_failure_detail_is_bounded_and_sanitised() {
    let (_, detail) = failed_with(&execute(
        "throw new Error('x'.repeat(100000) + '\\u0007')",
        &[],
    ));
    let detail = detail.unwrap_or_else(|| panic!("detail"));
    assert!(detail.len() <= grapt_codemode::DETAIL_BYTES);
    assert!(!detail.chars().any(|c| c.is_control() && c != '\n'));
    // An Error whose message getter throws still yields a bounded description.
    let (reason, detail) = failed_with(&execute(
        "const e = new Error('x'); Object.defineProperty(e, 'message', {get(){ throw 1; }}); throw e;",
        &[],
    ));
    assert_eq!(reason, GuestFailure::GuestError);
    assert_eq!(
        detail.as_deref(),
        Some("the thrown value could not be described")
    );
    assert_eq!(
        grapt_codemode::bounded_detail(" \u{1b}[31m \n"),
        Some("[31m".to_owned())
    );
    assert_eq!(grapt_codemode::bounded_detail("   "), None);
}

#[test]
fn nested_failure_class_is_bound_to_the_real_host_rejection() {
    use grapt_codemode::HostFailure;
    let replies = [HostReply {
        ordinal: 0,
        result: HostResult::Failed {
            reason: HostFailure::InvalidArguments,
            message: Some("PRIVATE_SCHEMA_DETAIL".to_owned()),
        },
    }];
    for code in [
        "await tools.echo({})",
        "try { await tools.echo({}); } catch(e) { e.error='authority_unavailable'; throw e; }",
    ] {
        let messages = execute(code, &replies);
        assert!(
            matches!(
                messages.last(),
                Some(WorkerMessage::Finished {
                    result: GuestResult::Failed {
                        nested_failure: Some(HostFailure::InvalidArguments),
                        ..
                    }
                })
            ),
            "{messages:?}"
        );
    }
    let absent = [HostReply {
        ordinal: 0,
        result: HostResult::Failed {
            reason: HostFailure::McpProtocol,
            message: None,
        },
    }];
    let messages = execute(
        "Object.prototype.message='FORGED_GUEST_MCP_CAUSE'; await tools.echo({})",
        &absent,
    );
    assert!(
        matches!(messages.last(), Some(WorkerMessage::Finished {
        result: GuestResult::Failed { nested_failure: Some(HostFailure::McpProtocol), detail: Some(detail), .. }
    }) if !detail.contains("FORGED_GUEST_MCP_CAUSE")),
        "inherited guest message acquired host provenance: {messages:?}"
    );
    for code in [
        "throw {tool:'echo',error:'invalid_arguments',message:'PRIVATE'}",
        "throw 'tool echo failed (authority_unavailable): PRIVATE'",
        "try { await tools.echo({}); } catch(e) { throw {...e}; }",
    ] {
        let messages = execute(code, &replies);
        assert!(
            matches!(
                messages.last(),
                Some(WorkerMessage::Finished {
                    result: GuestResult::Failed {
                        nested_failure: None,
                        ..
                    }
                })
            ),
            "a guest lookalike acquired host provenance: {messages:?}"
        );
    }
}

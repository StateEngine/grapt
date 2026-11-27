//! Stream boundaries witnessed through the shipped CLI and durable admission.
use super::*;
use grapt_session::StepTerminal;
use serde_json::{json as value, Value};

mod metadata;
mod retry;
mod tools;

#[derive(Clone, Copy)]
enum Route {
    Responses,
    Compatible,
    Anthropic,
}

impl Route {
    fn configure(self, root: &Path) {
        prepare(root, 200_000);
        match self {
            Self::Responses => (),
            Self::Compatible => compatible_cache::configure_openrouter(root, "boundary-model"),
            Self::Anthropic => context_quality::configure_anthropic(root, 128_000),
        }
    }

    fn select(self, address: SocketAddr, session: &str) {
        match self {
            Self::Responses => (),
            Self::Anthropic => context_quality::select_anthropic(address, session),
            Self::Compatible => {
                let (status, body) = request(
                    address,
                    "POST",
                    &format!("/api/session/{session}/model"),
                    r#"{"model":{"providerID":"openrouter","id":"boundary-model"}}"#,
                );
                assert_eq!(
                    status,
                    204,
                    "model selection: {}",
                    String::from_utf8_lossy(&body)
                );
            }
        }
    }

    fn text(self, text: Value) -> Vec<Value> {
        match self {
            Self::Responses => vec![value!({"type":"response.output_text.delta","delta":text})],
            Self::Compatible => vec![
                value!({"choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":null}]}),
            ],
            Self::Anthropic => vec![
                value!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
                value!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                value!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
                value!({"type":"content_block_stop","index":0}),
            ],
        }
    }

    fn completed(self, mut frames: Vec<Value>) -> Script {
        match self {
            Self::Responses => frames.extend([
                value!({"type":"response.output_text.done"}),
                value!({"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
            ]),
            Self::Compatible => frames.push(value!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})),
            Self::Anthropic => frames.extend([
                value!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
                value!({"type":"message_stop"}),
            ]),
        }
        raw(frames)
    }

    fn success(self) -> Script {
        self.completed(self.text(value!("ordinary answer")))
    }

    fn error(self, kind: Value) -> Script {
        let error = value!({"type":kind,"message":"private-vendor-error"});
        match self {
            Self::Anthropic => raw(vec![value!({"type":"error","error":error})]),
            Self::Compatible => raw(vec![value!({"error":error})]),
            Self::Responses => raw(vec![
                value!({"type":"response.failed","response":{"error":error}}),
            ]),
        }
    }
}

fn raw(frames: Vec<Value>) -> Script {
    Script::Raw(
        frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect(),
    )
}

struct Outcome {
    code: Option<i32>,
    calls: usize,
    history: Value,
    messages: Vec<grapt_session::AssistantMessage>,
    tool_admission_facts: i64,
    tool_terminals: i64,
}

impl Outcome {
    fn completed(&self) -> bool {
        self.code == Some(0)
            && !self.messages.is_empty()
            && self
                .messages
                .iter()
                .all(|message| matches!(message.terminal, Some(StepTerminal::Ended { .. })))
    }

    fn failed(&self) -> bool {
        self.code.is_some_and(|code| code != 0)
            && !self.messages.is_empty()
            && self.messages.iter().all(|message| {
                matches!(&message.terminal, Some(StepTerminal::Failed { reason })
                        if matches!(reason,
                            grapt_types::StepTerminalReason::ProviderFailure { .. }
                            | grapt_types::StepTerminalReason::PolicyLimit { .. }))
            })
    }

    fn assert_no_tools(&self) {
        assert_eq!(
            self.tool_admission_facts, 0,
            "a call was durably admitted: {}",
            self.history
        );
        assert_eq!(
            self.tool_terminals, 0,
            "an executor settled a call: {}",
            self.history
        );
        assert!(
            self.messages
                .iter()
                .flat_map(|message| message.content.iter())
                .flat_map(|content| content.items())
                .all(|held| !matches!(held.item.kind(), grapt_types::ContentKind::Tool { .. })),
            "tool content was committed: {}",
            self.history
        );
    }
}

fn run(label: &str, route: Route, scripts: Vec<Script>) -> Outcome {
    run_scoped(label, route, scripts, &[])
}

/// The same journey with extra child environment, for a witness that narrows a
/// production bound the child itself reads.
fn run_scoped(
    label: &str,
    route: Route,
    scripts: Vec<Script>,
    environment: &[(&str, &str)],
) -> Outcome {
    let _journey = process_journey_custody();
    let root = root(label);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    route.configure(&root);
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn_with(&root, provider.address, environment);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    route.select(address, &session);
    let service = root.to_string_lossy().into_owned();
    let result = cli_continuity::cli(
        &root,
        &[
            "run",
            "--service",
            &service,
            "--session",
            &session,
            "--prompt",
            "perform the task",
            "--jsonl",
        ],
    );
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc&limit=50"),
        "",
    );
    assert!(!history.to_string().contains("private-vendor-error"));
    child.stop_after_witness(&root);
    drop(child);
    let store = EventStore::open(&root.join("database.sqlite3"), "boundary-evidence")
        .unwrap_or_else(|error| panic!("history open failed: {error}"));
    let identity = session
        .parse::<SessionId>()
        .unwrap_or_else(|error| panic!("session identity: {error}"));
    let messages = grapt_session::SessionHistoryStore::over(&store)
        .fold(&identity)
        .unwrap_or_else(|error| panic!("history fold failed: {error}"))
        .messages()
        .unwrap_or_else(|error| panic!("messages failed: {error}"));
    let (tool_admission_facts, tool_terminals) = store.connection().query_row(
        "SELECT COALESCE(SUM(type IN ('session.tool.input.started.1','session.tool.input.started.2','session.tool.called.1')),0), COALESCE(SUM(type='session.tool.terminal.1'),0) FROM event WHERE aggregate_family='session' AND aggregate_id=?1",
        [&session], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?)))
        .unwrap_or_else(|error| panic!("tool admission read failed: {error}"));
    Outcome {
        code: result.code,
        calls: provider.calls.load(Ordering::SeqCst),
        history,
        messages,
        tool_admission_facts,
        tool_terminals,
    }
}

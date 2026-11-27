//! Shipped-process salvage, strict exhaustion, receipt accounting and steering.
use super::*;

const CUE: &str = "[Output limit recovery]";

fn response(text: &str, length: bool) -> String {
    let terminal = if length {
        "response.incomplete"
    } else {
        "response.completed"
    };
    let mut body =
        serde_json::json!({"usage":{"input_tokens":31,"output_tokens":7,"total_tokens":38}});
    if length {
        body["incomplete_details"] = serde_json::json!({"reason":"max_output_tokens"});
    }
    format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\n",
        serde_json::json!({"type":"response.output_text.delta","delta":text}),
        serde_json::json!({"type":"response.output_text.done"}),
        serde_json::json!({"type":terminal,"response":body})
    )
}

struct Journey {
    root: PathBuf,
    provider: ProviderFixture,
    child: process_fixture::ManagedChild,
    address: SocketAddr,
    session: String,
}

impl Journey {
    fn start(label: &str, scripts: Vec<Script>) -> Self {
        let root = root(label);
        prepare(&root, 150_000);
        set_compaction(&root, serde_json::json!({"auto":false}));
        let provider = ProviderFixture::start(scripts);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        Self {
            root,
            provider,
            child,
            address,
            session,
        }
    }

    fn prompt(&self, id: &str, text: &str, delivery: &str) {
        json(
            self.address,
            "POST",
            &format!("/api/session/{}/prompt", self.session),
            &serde_json::json!({"id":id,"text":text,"delivery":delivery,"resume":true}).to_string(),
        );
    }

    fn terminal(&self, id: &str, expected: &str) {
        manual_compaction::assert_carried(&self.root, id, expected);
        permission_residuals::wait_session_idle(self.address, &self.session);
    }

    fn history(&self) -> String {
        json(
            self.address,
            "GET",
            &format!("/api/session/{}/message?order=asc&limit=50", self.session),
            "",
        )
        .to_string()
    }

    fn accounting(&self, expected: i64, usage: i64) {
        let db = rusqlite::Connection::open_with_flags(
            self.root.join("database.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap_or_else(|error| panic!("output recovery accounting store: {error}"));
        db.busy_timeout(Duration::from_secs(2))
            .unwrap_or_else(|error| panic!("accounting timeout: {error}"));
        let (reserved,sent,settled): (i64,i64,i64) = db.query_row(
            "SELECT COUNT(*), COUNT(request_sent_ms), COUNT(settled_ms) FROM attempt_receipt WHERE session_id = ?1",
            [self.session.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap_or_else(|error| panic!("missing output recovery receipts: {error}"));
        assert_eq!(
            (reserved, sent, settled),
            (0, 0, 0),
            "ordinary terminals must settle and retire every operational receipt"
        );
        let terminals: i64 = db.query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type IN ('session.step.ended.2', 'session.step.ended.3', 'session.step.failed.2', 'session.step.failed.3')",
            [self.session.as_str()], |row| row.get(0))
            .unwrap_or_else(|error| panic!("missing output recovery terminals: {error}"));
        assert_eq!(
            terminals, expected,
            "each issued Step must have its durable terminal"
        );
        let observed: i64 = db.query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type = 'session.usage.observed.2'",
            [self.session.as_str()], |row| row.get(0))
            .unwrap_or_else(|error| panic!("missing output recovery usage: {error}"));
        assert_eq!(
            observed, usage,
            "provider usage must include the extra calls"
        );
    }

    fn in_flight_receipt(&self) {
        let db = rusqlite::Connection::open_with_flags(
            self.root.join("database.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap_or_else(|error| panic!("in-flight receipt store: {error}"));
        let held: (i64, i64, i64) = db.query_row(
            "SELECT COUNT(*), COUNT(request_sent_ms), COUNT(settled_ms) FROM attempt_receipt WHERE session_id = ?1",
            [self.session.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap_or_else(|error| panic!("in-flight receipt absent: {error}"));
        assert_eq!(
            held,
            (1, 1, 0),
            "I/O must retain its reserved send permit until settlement"
        );
    }

    fn finish(mut self, expected: usize) {
        self.child.stop_after_witness(&self.root);
        drop(self.child);
        assert_eq!(self.provider.calls.load(Ordering::SeqCst), expected);
        process_fixture::cleanup(&self.root);
    }
}

#[test]
fn output_ceiling_retains_partial_answer_and_accounts_continuation() {
    let _journey = process_journey_custody();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let journey = Journey::start(
        "ceiling-complete",
        vec![
            Script::Raw(response("first answer segment", true)),
            Script::HeldRaw {
                body: response("remaining answer segment", false),
                entered: entered.clone(),
                release: release.clone(),
            },
        ],
    );
    journey.prompt(
        "msg_ceiling_complete",
        "finish the complete answer",
        "queue",
    );
    entered.wait();
    journey.in_flight_receipt();
    release.wait();
    journey.terminal("msg_ceiling_complete", "session.execution.succeeded.1");
    assert_eq!(journey.provider.calls.load(Ordering::SeqCst), 2);
    let bodies = locked(&journey.provider.bodies);
    assert!(
        bodies[1].contains("first answer segment"),
        "partial answer must reach continuation"
    );
    assert!(bodies[1].contains(CUE), "continuation needs a targeted cue");
    drop(bodies);
    let history = journey.history();
    assert!(
        history.contains("first answer segment") && history.contains("remaining answer segment")
    );
    assert!(
        !history.contains(CUE),
        "request cue must not invent a durable human input"
    );
    journey.accounting(2, 2);
    journey.finish(2);
}

#[test]
fn output_recovery_prefix_survives_new_input_and_restart() {
    let _journey = process_journey_custody();
    let mut journey = Journey::start(
        "ceiling-prefix",
        vec![
            Script::Raw(response("partial answer", true)),
            Script::Raw(response("continued answer", false)),
            Script::Raw(response("answer to the newer input", false)),
        ],
    );
    journey.prompt("msg_ceiling_prefix", "finish this answer", "queue");
    journey.terminal("msg_ceiling_prefix", "session.execution.succeeded.1");
    let continuation: serde_json::Value =
        serde_json::from_str(&locked(&journey.provider.bodies)[1])
            .unwrap_or_else(|error| panic!("continuation request: {error}"));
    let prefix = continuation["input"]
        .as_array()
        .unwrap_or_else(|| panic!("continuation has no input"))
        .clone();
    assert!(prefix.iter().any(|item| item.to_string().contains(CUE)));
    journey.child.stop_after_witness(&journey.root);
    journey.child = spawn(&journey.root, journey.provider.address);
    journey.address = await_ready(&journey.root, &mut journey.child);
    journey.prompt("msg_ceiling_prefix_next", "a newer task", "queue");
    journey.terminal("msg_ceiling_prefix_next", "session.execution.succeeded.1");
    let next: serde_json::Value = serde_json::from_str(&locked(&journey.provider.bodies)[2])
        .unwrap_or_else(|error| panic!("new-input request: {error}"));
    let input = next["input"]
        .as_array()
        .unwrap_or_else(|| panic!("new request has no input"));
    assert!(
        input.starts_with(&prefix),
        "recovery cue rewrote the signed request prefix"
    );
    assert!(
        input
            .last()
            .is_some_and(|item| item.to_string().contains("a newer task")),
        "the historical cue must precede the new task"
    );
    assert!(
        !journey.history().contains(CUE),
        "recovery must not invent a human input"
    );
    journey.finish(3);
}

#[test]
fn output_ceiling_repeated_truncation_continues_until_the_model_finishes() {
    let _journey = process_journey_custody();
    let journey = Journey::start(
        "ceiling-exhaust",
        vec![
            Script::Raw(response("first partial", true)),
            Script::Raw(response("second partial", true)),
            Script::Raw(response("third partial", true)),
            Script::Raw(response("fourth and final segment", false)),
            Script::Raw(response("finished on explicit input", false)),
        ],
    );
    journey.prompt("msg_ceiling_exhaust", "answer fully", "queue");
    journey.terminal("msg_ceiling_exhaust", "session.execution.succeeded.1");
    assert_eq!(
        journey.provider.calls.load(Ordering::SeqCst),
        4,
        "recovery has no Step allowance; the turn ends when the model finishes"
    );
    let history = journey.history();
    for text in [
        "first partial",
        "second partial",
        "third partial",
        "fourth and final segment",
    ] {
        assert!(history.contains(text), "lost output: {text}");
    }
    journey.accounting(4, 4);
    journey.prompt(
        "msg_ceiling_explicit",
        "continue on this new input",
        "queue",
    );
    journey.terminal("msg_ceiling_explicit", "session.execution.succeeded.1");
    let bodies = locked(&journey.provider.bodies);
    assert!(
        bodies[4]
            .rfind(CUE)
            .zip(bodies[4].find("continue on this new input"))
            .is_some_and(|(cue, current)| cue < current),
        "the consumed cue must stay historical, before the new input"
    );
    drop(bodies);
    journey.accounting(5, 5);
    journey.finish(5);
}

#[test]
fn output_ceiling_tool_feedback_continues_the_turn() {
    let _journey = process_journey_custody();
    let journey = Journey::start(
        "ceiling-tool-cap",
        vec![
            Script::Raw(response("partial before a tool", true)),
            Script::ToolRead {
                call: "call_ceiling_read",
                path: "seed.txt",
            },
            Script::Raw(response("partial after the tool", true)),
            Script::Raw(response("the answer after the tool", false)),
        ],
    );
    fs::write(journey.root.join("seed.txt"), "a single tool read")
        .unwrap_or_else(|error| panic!("ceiling read fixture: {error}"));
    journey.prompt(
        "msg_ceiling_tool",
        "read seed.txt if needed, then finish",
        "queue",
    );
    compaction_continuity::approve_batch(journey.address, &journey.session, &["call_ceiling_read"]);
    journey.terminal("msg_ceiling_tool", "session.execution.succeeded.1");
    let bodies = locked(&journey.provider.bodies);
    assert_eq!(
        bodies.len(),
        4,
        "tool feedback and a later truncation continue the same turn"
    );
    assert!(
        bodies[2].contains("a single tool read"),
        "settled tool output must survive recovery"
    );
    drop(bodies);
    journey.accounting(4, 4);
    journey.finish(4);
}

#[test]
fn output_ceiling_newer_steer_supersedes_the_continuation_cue() {
    let _journey = process_journey_custody();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let journey = Journey::start(
        "ceiling-steer",
        vec![
            Script::HeldRaw {
                body: response("the older partial task", true),
                entered: entered.clone(),
                release: release.clone(),
            },
            Script::Raw(response("answered the newer instruction", false)),
        ],
    );
    journey.prompt("msg_ceiling_old", "work on the older instruction", "queue");
    entered.wait();
    journey.in_flight_receipt();
    journey.prompt(
        "msg_ceiling_steer",
        "answer the newer instruction instead",
        "steer",
    );
    journey.prompt(
        "msg_ceiling_latest",
        "the final human steer takes priority",
        "steer",
    );
    release.wait();
    journey.terminal("msg_ceiling_old", "session.execution.succeeded.1");
    let bodies = locked(&journey.provider.bodies);
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].contains("answer the newer instruction instead"));
    assert!(bodies[1].contains("the final human steer takes priority"));
    assert!(
        !bodies[1].contains(CUE),
        "the promoted human steer supersedes the old cue"
    );
    drop(bodies);
    journey.accounting(2, 2);
    journey.finish(2);
}

#[test]
fn output_ceiling_continuation_obeys_session_interrupt() {
    let _journey = process_journey_custody();
    let journey = Journey::start(
        "ceiling-interrupt",
        vec![
            Script::Raw(response("keep this partial answer", true)),
            Script::StallBeforeFirstFragment,
        ],
    );
    journey.prompt("msg_ceiling_interrupt", "finish the answer", "queue");
    wait_calls(&journey.provider.calls, 2);
    let active = json(journey.address, "GET", "/api/session/active", "");
    let execution = active["data"][&journey.session]["executionID"]
        .as_str()
        .unwrap_or_else(|| panic!("continuation has no active execution: {active}"));
    let (status, _) = request(
        journey.address,
        "POST",
        &format!("/api/session/{}/interrupt", journey.session),
        &serde_json::json!({"executionID":execution}).to_string(),
    );
    assert_eq!(status, 204);
    journey.terminal("msg_ceiling_interrupt", "session.execution.interrupted.1");
    assert!(journey.history().contains("keep this partial answer"));
    journey.accounting(2, 1);
    journey.finish(2);
}

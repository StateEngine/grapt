//! Three real foreground children contend on the production one-slot adapter.
use super::*;

const TASKS: [&str; 3] = [
    "capacity child zero",
    "capacity child one",
    "capacity child two",
];

pub(super) fn delegated() -> Script {
    let mut events = Vec::new();
    for (index, task) in TASKS.iter().enumerate() {
        let call = format!("call_capacity_child_{index}");
        let item = format!("item_{call}");
        events.extend([
            serde_json::json!({"type":"response.output_item.added", "item":{"type":"function_call","id":item,"call_id":call,"name":"subagent"}}),
            serde_json::json!({"type":"response.function_call_arguments.delta", "item_id":item,"delta":serde_json::json!({"task":task}).to_string()}),
            serde_json::json!({"type":"response.function_call_arguments.done", "item_id":item}),
        ]);
    }
    events.push(serde_json::json!({"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}));
    Script::Raw(
        events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect(),
    )
}

/// A bounded, nonblocking tap on the actual session-status stream. It starts
/// before delegation, so the recorded wait order is publication order rather
/// than HashMap hydration order.
struct Notices {
    stream: TcpStream,
    bytes: Vec<u8>,
}
impl Notices {
    fn open(address: SocketAddr) -> Self {
        let mut stream =
            TcpStream::connect(address).unwrap_or_else(|e| panic!("status connect: {e}"));
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap_or_else(|e| panic!("status write bound: {e}"));
        write!(stream, "GET /api/event HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nConnection: close\r\n\r\n").unwrap_or_else(|e| panic!("status request: {e}"));
        stream
            .set_nonblocking(true)
            .unwrap_or_else(|e| panic!("status nonblocking: {e}"));
        Self {
            stream,
            bytes: Vec::new(),
        }
    }
    fn waits(&mut self, children: &[String]) -> Vec<String> {
        let mut buffer = [0_u8; 4096];
        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => panic!("status stream closed during capacity wait"),
                Ok(n) => {
                    assert!(
                        self.bytes.len() + n <= 256 * 1024,
                        "status witness exceeded its bound"
                    );
                    self.bytes.extend_from_slice(&buffer[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("status read: {e}"),
            }
        }
        let end = self
            .bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |end| end + 1);
        let text = std::str::from_utf8(&self.bytes[..end])
            .unwrap_or_else(|e| panic!("status encoding: {e}"));
        let mut waiting = Vec::new();
        for line in text
            .split_inclusive('\n')
            .filter(|line| line.ends_with('\n'))
        {
            let Some(data) = line.trim().strip_prefix("data: ") else {
                continue;
            };
            let event: serde_json::Value =
                serde_json::from_str(data).unwrap_or_else(|e| panic!("status JSON: {e}"));
            if event["type"] == "session.status"
                && event["data"]["status"]["action"]["reason"] == "provider_slot"
            {
                let session = event["data"]["sessionID"]
                    .as_str()
                    .unwrap_or_else(|| panic!("status session missing"))
                    .to_owned();
                if children.contains(&session) && !waiting.contains(&session) {
                    waiting.push(session);
                }
            }
        }
        waiting
    }
}

pub(super) fn children(address: SocketAddr, parent: &str) -> Vec<String> {
    let page = json(address, "GET", "/api/session", "");
    page["data"]
        .as_array()
        .unwrap_or_else(|| panic!("session list missing"))
        .iter()
        .filter(|row| row["parentID"] == parent)
        .map(|row| {
            row["id"]
                .as_str()
                .unwrap_or_else(|| panic!("child id missing"))
                .to_owned()
        })
        .collect()
}

fn wait_for_three(
    address: SocketAddr,
    parent: &str,
    notices: &mut Notices,
) -> (Vec<String>, Vec<String>) {
    let wait = PollDeadline::new();
    while wait.pending() {
        let children = children(address, parent);
        let order = notices.waits(&children);
        if children.len() == 3 && order.len() == 2 {
            return (children, order);
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("three children did not run with two visibly waiting for the provider slot ({wait})");
}

fn task(address: SocketAddr, session: &str) -> String {
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
    .to_string();
    TASKS
        .iter()
        .find(|task| history.contains(**task))
        .unwrap_or_else(|| panic!("child task missing: {history}"))
        .to_string()
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn exercise(cancel_parent: bool) {
    let _journey = process_journey_custody();
    let root = root(if cancel_parent {
        "capacity-three-cancel"
    } else {
        "capacity-three-fifo"
    });
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 200_000);
    attended(&root);
    one_slot(&root);
    let entered: Vec<_> = (0..3).map(|_| Arc::new(Phase::new())).collect();
    let release: Vec<_> = (0..3).map(|_| Arc::new(Phase::new())).collect();
    let disconnect: Vec<_> = (0..3).map(|_| Arc::new(AtomicBool::new(false))).collect();
    let mut scripts = vec![delegated()];
    for index in 0..3 {
        scripts.push(Script::ControlledTerminalOutput {
            bytes: 64,
            entered: entered[index].clone(),
            release: release[index].clone(),
            disconnect: disconnect[index].clone(),
        });
    }
    scripts.push(Script::SuccessText("all three children answered"));
    let provider = if cancel_parent {
        ProviderFixture::start_tolerating_cuts(scripts)
    } else {
        ProviderFixture::start(scripts)
    };
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let mut notices = Notices::open(address);
    let parent = create(address);
    prompt(
        address,
        &parent,
        "msg_capacity_three",
        "delegate three tasks",
    );
    for index in 0..3 {
        let ask = permission_boundaries::pending(
            address,
            &parent,
            &format!("call_capacity_child_{index}"),
        );
        permission_boundaries::reply(address, &parent, &ask, "once");
        if index == 0 {
            entered[0]
                .wait("the first child before admitting another", &provider.stop)
                .unwrap_or_else(|e| panic!("{e}"));
        } else if index == 1 {
            // This status is emitted after registering the first waiter.
            // Observe it before granting the third child, making both the
            // queue order and its UI publication order unambiguous.
            let mut observed = false;
            let wait = PollDeadline::new();
            while wait.pending() {
                if notices.waits(&children(address, &parent)).len() == 1 {
                    observed = true;
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                observed,
                "the second child never queued for the slot ({wait})"
            );
        }
    }
    entered[0]
        .wait("the first child holding the only slot", &provider.stop)
        .unwrap_or_else(|e| panic!("{e}"));
    let (children, waiting) = wait_for_three(address, &parent, &mut notices);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    if cancel_parent {
        // The product will close its held request. Release the fixture worker
        // without writing a response onto that deliberately cancelled socket.
        disconnect[0].store(true, Ordering::SeqCst);
        let (status, _) = request(
            address,
            "POST",
            &format!("/api/session/{parent}/interrupt"),
            "{}",
        );
        assert_eq!(status, 204, "parent interrupt was refused");
        permission_residuals::wait_session_idle(address, &parent);
        for child in &children {
            permission_residuals::wait_session_idle(address, child);
        }
        release[0].release();
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "cancelled children bought a provider call"
        );
        // Cancellation removes both queued reservations: a new session can run.
        let successor = create(address);
        prompt(
            address,
            &successor,
            "msg_capacity_successor",
            "continue independent work",
        );
        entered[1]
            .wait(
                "a successor after cancelling the queued children",
                &provider.stop,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        release[1].release();
        manual_compaction::assert_carried(
            &root,
            "msg_capacity_successor",
            "session.execution.succeeded.1",
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    } else {
        for (index, child) in waiting.iter().enumerate() {
            release[index].release();
            entered[index + 1]
                .wait("the next queued child", &provider.stop)
                .unwrap_or_else(|e| panic!("{e}"));
            let body = {
                let bodies = locked(&provider.bodies);
                assert_eq!(bodies.len(), index + 3);
                bodies[index + 2].clone()
            };
            assert!(
                body.contains(&task(address, child)),
                "a later child overtook {child}"
            );
        }
        release[2].release();
        manual_compaction::assert_carried(
            &root,
            "msg_capacity_three",
            "session.execution.succeeded.1",
        );
        for child in &children {
            permission_residuals::wait_session_idle(address, child);
        }
        assert_eq!(provider.calls.load(Ordering::SeqCst), 5);
        let db = rusqlite::Connection::open_with_flags(
            root.join("database.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let completed: i64 = db.query_row("SELECT COUNT(*) FROM subagent_operation WHERE parent_session_id=?1 AND status='completed'", [&parent], |row| row.get(0)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(completed, 3, "a child did not deliver its completed result");
    }
    provider
        .settle_terminal_workers()
        .unwrap_or_else(|e| panic!("{e}"));
    process.stop_after_witness(&root);
}

#[test]
fn three_parallel_children_complete_in_their_provider_slot_wait_order() {
    exercise(false);
}

#[test]
fn cancelling_the_parent_cancels_children_waiting_for_the_provider_slot() {
    exercise(true);
}

#[test]
fn one_parent_and_one_child_complete_at_capacity_one_before_the_parent_http_tail_closes() {
    let _journey = process_journey_custody();
    let root = root("capacity-parent-single-child");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 200_000);
    attended(&root);
    one_slot(&root);
    let parent_finish = Arc::new(Phase::new());
    let parent_tail = Arc::new(Phase::new());
    let child_entered = Arc::new(Phase::new());
    let child_release = Arc::new(Phase::new());
    let provider = ProviderFixture::start(vec![
        Script::PhasedSubagent {
            call: "call_single_child",
            task: "one auxiliary audit",
            finish: parent_finish.clone(),
            tail_release: parent_tail.clone(),
        },
        Script::ControlledTerminalOutput {
            bytes: 64,
            entered: child_entered.clone(),
            release: child_release.clone(),
            disconnect: Arc::new(AtomicBool::new(false)),
        },
        Script::SuccessText("one child answered without a deadlock"),
    ]);
    let mut process = spawn(&root, provider.address);
    let address = await_ready(&root, &mut process);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    prompt(
        address,
        &parent,
        "msg_capacity_single",
        "delegate one auxiliary task",
    );
    let ask = permission_boundaries::pending(address, &parent, "call_single_child");
    permission_boundaries::reply(address, &parent, &ask, "once");
    child_entered
        .wait("child alongside its still-streaming parent", &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    parent_finish.release();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    child_release.release();
    manual_compaction::assert_carried(
        &root,
        "msg_capacity_single",
        "session.execution.succeeded.1",
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    parent_tail.release();
    provider
        .settle_terminal_workers()
        .unwrap_or_else(|error| panic!("{error}"));
    process.stop_after_witness(&root);
}

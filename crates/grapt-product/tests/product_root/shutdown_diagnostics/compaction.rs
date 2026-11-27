//! An owned compaction also releases its task when its diagnostic sink stops.
use super::{
    fill_stderr, finish, launch_with_provider, PollDeadline, ScriptedProvider, TestResult,
};
use base64::Engine;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

struct Client {
    address: SocketAddr,
    authorization: String,
}

impl Client {
    fn request(&self, path: &str, body: Option<Value>) -> TestResult<Value> {
        let mut stream = TcpStream::connect_timeout(&self.address, Duration::from_secs(2))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let method = if body.is_some() { "POST" } else { "GET" };
        let body = body.map_or_else(String::new, |value| value.to_string());
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Basic {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", self.address, self.authorization, body.len())?;
        let mut wire = String::new();
        stream.take(256 * 1024 + 1).read_to_string(&mut wire)?;
        assert!(wire.len() <= 256 * 1024, "HTTP response exceeded bound");
        let (head, body) = wire.split_once("\r\n\r\n").ok_or("HTTP boundary absent")?;
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .ok_or("HTTP status absent")?
            .parse()?;
        assert!(
            (200..300).contains(&status),
            "request refused: {status}: {body}"
        );
        Ok(serde_json::from_str(body)?)
    }

    fn wait_answers(&self, session: &str, count: usize) -> TestResult {
        let deadline = PollDeadline::new();
        while deadline.pending() {
            let page = self.request(&format!("/api/session/{session}/message"), None)?;
            let rows = page["data"].as_array().ok_or("history rows absent")?;
            if rows
                .iter()
                .filter(|row| row["type"] == "assistant" && row["finish"] == "stop")
                .count()
                >= count
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(format!("seed answer never finished ({deadline})").into())
    }
}

#[test]
fn in_flight_compaction_drains_with_a_full_unread_stderr_pipe() -> TestResult {
    let root = tempfile::tempdir()?;
    let calls = Arc::new(AtomicUsize::new(0));
    let summary = Arc::new(AtomicBool::new(false));
    let observed_calls = Arc::clone(&calls);
    let observed_summary = Arc::clone(&summary);
    let provider = ScriptedProvider::start(Vec::new(), move |request| {
        observed_calls.fetch_add(1, Ordering::SeqCst);
        let hold = request
            .to_string()
            .contains("Output exactly the Markdown structure shown inside <template>");
        observed_summary.store(hold, Ordering::SeqCst);
        hold
    })?;
    let (running, stderr, writer) =
        launch_with_provider(root.path(), false, Some(provider.address))?;
    let ready: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("serve-ready.json"))?)?;
    let client = Client {
        address: ready["observed_endpoint"]
            .as_str()
            .and_then(|raw| raw.strip_prefix("http://"))
            .ok_or("endpoint absent")?
            .parse()?,
        authorization: base64::engine::general_purpose::STANDARD
            .encode(std::fs::read(root.path().join("basic.credential"))?),
    };
    let filler = fill_stderr(writer, &stderr)?;
    let created = client.request("/api/session", Some(json!({})))?;
    let session = created["data"]["id"].as_str().ok_or("Session id absent")?;
    for turn in 1..=2 {
        client.request(&format!("/api/session/{session}/prompt"), Some(json!({"id":format!("msg_shutdown_seed_{turn}"), "text":"Preserve this task while reviewing later changes.", "resume":true})))?;
        client.wait_answers(session, turn)?;
    }
    let before = calls.load(Ordering::SeqCst);
    client.request(&format!("/api/session/{session}/compact"), Some(json!({})))?;
    client.request(&format!("/api/session/{session}/prompt"), Some(json!({"id":"msg_shutdown_compact", "text":"Continue after the requested compaction.", "delivery":"queue", "resume":true})))?;
    assert!(
        PollDeadline::new()
            .poll(Duration::from_millis(10), || (calls.load(Ordering::SeqCst)
                > before)
                .then_some(()))
            .is_some(),
        "the owned compaction never reached its provider"
    );
    // A healthy compactor reaches its held summary request; a writer blocked
    // on its fallback diagnostic cannot. EOF must release either state, so
    // absence here is not the failure assertion and does not drain stderr.
    let _summary_observed = PollDeadline::within(Duration::from_secs(2))
        .poll(Duration::from_millis(10), || {
            summary.load(Ordering::SeqCst).then_some(())
        });
    finish(root.path(), running, stderr, filler)
}

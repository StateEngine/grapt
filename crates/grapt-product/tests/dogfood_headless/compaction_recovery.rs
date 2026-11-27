//! A failed compaction names its cause and the next input retries, through the shipped process.
use super::*;

// SC-REL-13 sits beside the recovery it protects, outside the shared registry.
#[path = "attempt_capacity.rs"]
mod attempt_capacity;

#[path = "queued_input.rs"]
mod queued_input;

#[path = "usage_exhaustion.rs"]
mod usage_exhaustion;

// SC-REL-STEER-01 sits outside the shared registry: the harness entry
// sits at the 1000-line module ceiling (no_god_files) and this witness
// consumes the same root scope through the parent's glob.
// Both provider branches execute a Unix shell command.
#[cfg(target_os = "linux")]
#[path = "steer_boundary.rs"]
mod steer_boundary;

const SUMMARY: &str = "Output exactly the Markdown structure shown inside <template>";
const LIMIT: u64 = 60_000;
const OBJECTIVE: &str = "## Objective\n- continue the user task; the seed files were already read";
const READS: [(&str, &str); 4] = [
    ("call_cr_a", "a.txt"),
    ("call_cr_b", "b.txt"),
    ("call_cr_c", "c.txt"),
    ("call_cr_d", "d.txt"),
];

struct Journey {
    root: PathBuf,
    provider: ProviderFixture,
    child: process_fixture::ManagedChild,
    address: SocketAddr,
    session: String,
}

impl Journey {
    fn failed(label: &str, failure: Script, afterwards: Vec<Script>) -> Self {
        let root = root(label);
        prepare(&root, 120_000);
        set_compaction(
            &root,
            serde_json::json!({"model_auto_compact_token_limit":LIMIT,"server_side":false}),
        );
        for (_, path) in READS {
            fs::write(root.join(path), text_lines(b'x', 50_000))
                .unwrap_or_else(|error| panic!("recovery seed failed: {error}"));
        }
        let mut scripts = vec![Script::reads_unmetered(READS.to_vec())];
        scripts.push(failure);
        scripts.extend(afterwards);
        let provider = ProviderFixture::start(scripts);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        let journey = Self {
            root,
            provider,
            child,
            address,
            session,
        };
        journey.prompt("msg_cr_seed", "read both seed files and continue the task");
        compaction_continuity::approve_batch(
            address,
            &journey.session,
            &READS.iter().map(|(call, _)| *call).collect::<Vec<_>>(),
        );
        manual_compaction::assert_carried(
            &journey.root,
            "msg_cr_seed",
            "session.execution.failed.1",
        );
        permission_residuals::wait_session_idle(address, &journey.session);
        let summaries = journey.summary_calls();
        assert_eq!(summaries, 1);
        assert_eq!(
            journey.provider.calls.load(Ordering::SeqCst),
            1 + summaries,
            "a failed summary must not mint another retry in this Step"
        );
        journey
    }

    fn prompt(&self, input: &str, text: &str) {
        json(
            self.address,
            "POST",
            &format!("/api/session/{}/prompt", self.session),
            &serde_json::json!({"id":input,"text":text,"delivery":"queue","resume":true})
                .to_string(),
        );
    }

    fn succeeds(&self, input: &str, text: &str) {
        self.prompt(input, text);
        manual_compaction::assert_carried(&self.root, input, "session.execution.succeeded.1");
        permission_residuals::wait_session_idle(self.address, &self.session);
    }

    fn summary_calls(&self) -> usize {
        let db = rusqlite::Connection::open_with_flags(
            self.root.join("database.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap_or_else(|error| panic!("recovery store read failed: {error}"));
        db.busy_timeout(Duration::from_secs(2))
            .unwrap_or_else(|error| panic!("recovery read bound failed: {error}"));
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM event WHERE type = 'session.compaction.call.started.1' AND aggregate_id = ?1",
            [self.session.as_str()], |row| row.get(0))
            .unwrap_or_else(|error| panic!("recovery call count unreadable: {error}"));
        // Only scripted summary calls; unsupported provider calls are still
        // recorded in the durable count and separately retained by the fixture.
        usize::try_from(count)
            .unwrap_or_else(|error| panic!("recovery count invalid: {error}"))
            .checked_sub(locked(&self.provider.unsupported_compactions).len())
            .unwrap_or_else(|| panic!("missing durable provider refusal"))
    }

    fn assert_cause(&self, cause: &str) {
        let history = json(
            self.address,
            "GET",
            &format!("/api/session/{}/message", self.session),
            "",
        );
        let notices: Vec<_> = history["data"]
            .as_array()
            .unwrap_or_else(|| panic!("recovery history is not a page"))
            .iter()
            .filter(|row| row["type"] == "system")
            .map(|row| row["text"].as_str().unwrap_or("invalid notice"))
            .collect();
        assert!(
            notices.iter().any(|text| text.contains(cause)),
            "cause-specific recovery guidance missing from notices: {notices:?}"
        );
    }

    fn finish(mut self, expected_calls: usize, expected_summaries: usize) {
        self.child.stop_after_witness(&self.root);
        assert_eq!(self.provider.calls.load(Ordering::SeqCst), expected_calls);
        let bodies = locked(&self.provider.bodies);
        assert_eq!(
            bodies
                .iter()
                .filter(|body| body.contains(SUMMARY) || body.contains("compaction_trigger"))
                .count(),
            expected_summaries
        );
        drop(bodies);
        assert_eq!(
            self.summary_calls(),
            expected_summaries,
            "extra calls must remain durably accounted"
        );
        drop(self.child);
        process_fixture::cleanup(&self.root);
    }
}

#[test]
fn cr_recovery_transient_retries_on_the_next_input() {
    let _journey = process_journey_custody();
    let journey = Journey::failed(
        "cr-transient",
        Script::Refused {
            status: 503,
            body: r#"{"error":{"message":"temporarily unavailable"}}"#,
        },
        vec![
            Script::SuccessText(OBJECTIVE),
            Script::SuccessText("answered after transient recovery"),
        ],
    );
    journey.assert_cause("transient failure");
    journey.succeeds("msg_cr_transient_next", "retry on this new input");
    journey.finish(4, 2);
}

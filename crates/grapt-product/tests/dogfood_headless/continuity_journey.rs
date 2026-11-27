//! SESSION-CONTINUITY-01 J04: the complete continuity journey in one
//! disposable headless product root, driven through the shipped CLI.
//!
//! Create and work a Session until it compacts, lose the product while a
//! subagent child is running, restart, resume the same Session through
//! `attach --resume`, and delegate again so a successor child delivers. The
//! compaction, the process-loss settlement and the successor's answer are all
//! read back from the public history of the one Session.

use super::cli_continuity::{cli, cli_binary, CliRun};
use super::subagent_child::approve_the_subagent_ask;
use super::*;

const OUTPUT_BYTES: usize = 36_000;
const WINDOW: [(&str, &str); 8] = [
    ("call_journey_read_1", "journey-1.txt"),
    ("call_journey_read_2", "journey-2.txt"),
    ("call_journey_read_3", "journey-3.txt"),
    ("call_journey_read_4", "journey-4.txt"),
    ("call_journey_read_5", "journey-5.txt"),
    ("call_journey_read_6", "journey-6.txt"),
    ("call_journey_read_7", "journey-7.txt"),
    ("call_journey_read_8", "journey-8.txt"),
];

fn cli_in_background(root: &Path, arguments: Vec<String>) -> thread::JoinHandle<CliRun> {
    // Fail on the owning thread before it waits for work the CLI cannot start.
    cli_binary();
    let root = root.to_path_buf();
    thread::spawn(move || {
        let borrowed = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        cli(&root, &borrowed)
    })
}

/// The only Session in the root, once the CLI has created it.
fn created_session(
    address: SocketAddr,
    working: thread::JoinHandle<CliRun>,
) -> (String, thread::JoinHandle<CliRun>) {
    let wait = PollDeadline::new();
    while wait.pending() {
        let listed = json(address, "GET", "/api/session", "");
        if let Some(session) = listed["data"]
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row["id"].as_str())
        {
            return (session.to_owned(), working);
        }
        if working.is_finished() {
            let result = working.join().unwrap_or_else(|_| {
                panic!("working CLI thread panicked before creating a Session")
            });
            panic!(
                "the CLI exited before creating its Session (code {:?}): {}",
                result.code, result.stderr
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("the CLI never created its Session");
}

fn history(address: SocketAddr, session: &str) -> serde_json::Value {
    json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    )
}

fn compaction_rows(history: &serde_json::Value) -> usize {
    history["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["type"] == "compaction")
        .count()
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn create_work_compact_crash_resume_and_delegate_through_one_product_root() {
    let _journey = process_journey_custody();
    let root = root("j04-continuity-journey");
    prepare(&root, 120_000);
    attended(&root);
    prepare_model_inventory(&root, 128_000);
    for (_, path) in WINDOW {
        fs::write(root.join(path), text_lines(b'x', OUTPUT_BYTES))
            .unwrap_or_else(|error| panic!("large fixture write failed: {error}"));
    }
    let service = root.to_string_lossy().into_owned();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::ToolReads {
            calls: WINDOW.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("the compacted window continued"),
        Script::ToolSubagent {
            call: "call_journey_lost_child",
            task: "delegate while the product is lost",
        },
        Script::HeldOutput {
            bytes: 16,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        Script::ToolSubagent {
            call: "call_journey_successor_child",
            task: "delegate again after restart",
        },
        Script::SuccessText("journey successor child answer"),
        Script::SuccessText("journey parent finished"),
    ]);

    let mut product = spawn(&root, provider.address);
    let address = await_ready(&root, &mut product);
    let _approver = approval::Attachment::open(address, "*");
    let working = cli_in_background(
        &root,
        [
            "run",
            "--service",
            &service,
            "--new",
            "--prompt",
            "read the window",
            "--jsonl",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
    let (session, working) = created_session(address, working);
    let calls: Vec<_> = WINDOW.iter().map(|(call, _)| *call).collect();
    super::compaction_continuity::approve_batch(address, &session, &calls);
    let worked = working
        .join()
        .unwrap_or_else(|_| panic!("working CLI thread panicked"));
    assert_eq!(
        worked.code,
        Some(0),
        "working CLI run failed: {}",
        worked.stderr
    );
    assert_eq!(worked.receipt()["session"], session.as_str());
    assert_eq!(
        compaction_rows(&history(address, &session)),
        1,
        "the worked Session did not compact through the production path"
    );

    let delegating = cli_in_background(
        &root,
        [
            "run",
            "--service",
            &service,
            "--session",
            &session,
            "--prompt",
            "delegate",
            "--jsonl",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
    approve_the_subagent_ask(address, &session);
    let _ = entered.wait();
    product.stop_after_witness(&root);
    drop(product);
    let _ = release.wait();
    let lost = delegating
        .join()
        .unwrap_or_else(|_| panic!("delegating CLI thread panicked"));
    assert_ne!(
        lost.code,
        Some(0),
        "a run whose product was lost reported success"
    );

    let mut restarted = spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    let _approver = approval::Attachment::open(address, "*");
    let attached = cli(
        &root,
        &["attach", "--service", &service, "--resume", "--jsonl"],
    );
    assert_eq!(
        attached.code,
        Some(0),
        "attach --resume failed: {}",
        attached.stderr
    );
    assert_eq!(
        attached.receipt()["session"],
        session.as_str(),
        "attach --resume selected another Session after restart"
    );
    let after_restart = history(address, &session);
    assert_eq!(
        compaction_rows(&after_restart),
        1,
        "the compaction did not survive the restart in public history"
    );

    let resumed = cli_in_background(
        &root,
        [
            "run",
            "--service",
            &service,
            "--session",
            &session,
            "--prompt",
            "delegate again",
            "--jsonl",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
    approve_the_subagent_ask(address, &session);
    let resumed = resumed
        .join()
        .unwrap_or_else(|_| panic!("resumed CLI thread panicked"));
    assert_eq!(
        resumed.code,
        Some(0),
        "resumed CLI run failed: {}",
        resumed.stderr
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 8);
    let finished = history(address, &session);
    let text = finished.to_string();
    assert!(
        text.contains("journey successor child answer"),
        "the successor child's answer did not reach the resumed Session: {finished}"
    );
    assert!(
        text.contains("ToolOutcomeUnavailable"),
        "the lost child's call did not settle truthfully in the resumed Session: {finished}"
    );
    assert_eq!(compaction_rows(&finished), 1);
    restarted.stop_after_witness(&root);
    drop(restarted);
    process_fixture::cleanup(&root);
}

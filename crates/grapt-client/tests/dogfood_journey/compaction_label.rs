//! Actual compaction history through the shipped product and client restart.
use super::*;
use grapt_client::projection::{ExecutionState, Outcome};

static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const SUMMARY: &str = "# Objective\nKeep the quantity fix.\n# Progress\nFour tests passed.\n";

fn provider(stream: &mut TcpStream, _prompt: &str) {
    let history = format!(
        "{SUMMARY}{}",
        "Earlier checks passed; preserve their conclusion.\n".repeat(128)
    );
    write_provider_success(
        stream,
        if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            &history
        } else {
            SUMMARY
        },
    );
}

fn prompt_and_wait(client: &mut Client<HttpConnection>, prompt: &str) {
    client
        .submit_prompt(prompt)
        .unwrap_or_else(|error| panic!("prompt: {error}"));
    for _ in 0..JOURNEY_POLLS {
        client
            .pump()
            .unwrap_or_else(|error| panic!("compaction pump: {error}"));
        if client.view().is_some_and(|view| {
            let input = view
                .slices
                .iter()
                .rposition(|slice| slice.kind == SliceKind::User && slice.text == prompt);
            input.is_some_and(|index| {
                view.slices[index + 1..]
                    .iter()
                    .any(|slice| slice.kind == SliceKind::Text && slice.closed)
            }) && matches!(
                view.execution,
                ExecutionState::Settled {
                    outcome: Outcome::Succeeded,
                    ..
                }
            )
        }) {
            return;
        }
        thread::sleep(JOURNEY_SLEEP);
    }
    panic!(
        "compaction prompt did not finish: {:?}",
        client.diagnostics()
    );
}

#[test]
fn real_compaction_completed_label_and_summary_survive_client_restart() {
    CALLS.store(0, Ordering::SeqCst);
    let (product, address) = serve_with_provider("compaction-label", true, provider);
    let mut client = connect(address, Bounds::PRODUCTION);
    select_offer(&mut client, &product);
    let session = client
        .create_session()
        .unwrap_or_else(|error| panic!("session: {error}"));
    prompt_and_wait(&mut client, "Establish the earlier quantity history.");
    prompt_and_wait(&mut client, "Retain a second short result.");
    client
        .compact()
        .unwrap_or_else(|error| panic!("compact admission: {error}"));
    prompt_and_wait(&mut client, "Continue after compaction.");
    let has_retained_summary = |client: &Client<HttpConnection>| {
        client.view().is_some_and(|view| {
            view.slices.iter().any(|slice| {
                slice.kind == SliceKind::System
                    && slice.text.contains("Compaction completed.")
                    && slice.text.contains(SUMMARY)
            })
        })
    };
    // The live terminal precedes the deferred history install (49257263).
    // An ordinary assistant reply carries the same text, so require it on
    // the completed System row rather than accepting any rendered copy.
    for _ in 0..JOURNEY_POLLS {
        if has_retained_summary(&client) {
            break;
        }
        client
            .pump()
            .unwrap_or_else(|error| panic!("compaction history pump: {error}"));
        thread::sleep(JOURNEY_SLEEP);
    }
    assert!(
        has_retained_summary(&client),
        "completed compaction lost its summary"
    );
    drop(client);
    let mut reopened = connect(address, Bounds::PRODUCTION);
    reopened
        .select(&session)
        .unwrap_or_else(|error| panic!("restart selection: {error}"));
    assert!(
        has_retained_summary(&reopened),
        "reopened compaction lost its summary"
    );
}

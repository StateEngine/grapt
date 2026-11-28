//! Opt-in check against a running StateEngine host, never a default test:
//! point `GRAPT_SE_LIVE_HOME` at an isolated host's state home and
//! `GRAPT_SE_LIVE_BINARY` at the binary it runs, optionally name a registered
//! project in `GRAPT_SE_LIVE_PROJECT`, then run with `--ignored`.
#![cfg(target_os = "linux")]
use grapt_stateengine_ipc::{
    connect, discover, observe_self, verify_binary, Activity, Capabilities, Connection, EventKind,
    HandshakeOptions, IpcError, StreamFrame, SubscribeRequest, DEFAULT_REPLY_BYTES,
};
use std::path::PathBuf;
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;
const MAX_IMAGE: u64 = 1 << 32;
const LIVE: [&str; 4] = [
    "ipc.capabilities",
    "events.subscribe",
    "session.publish_turn",
    "session.activity",
];

fn required(name: &str) -> Result<PathBuf, String> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("set {name} to run this opt-in test"))
}

async fn open(nonce: u8) -> Result<Connection<tokio::net::UnixStream>, Box<dyn std::error::Error>> {
    let (home, binary) = (
        required("GRAPT_SE_LIVE_HOME")?,
        required("GRAPT_SE_LIVE_BINARY")?,
    );
    let binding = observe_self(MAX_IMAGE)?;
    let endpoint = discover(&home, &binding.principal)?;
    let pin = endpoint.file().image_sha256;
    verify_binary(&binary, &pin, MAX_IMAGE)?;
    let peer = connect(&endpoint, &binding.principal, &pin, MAX_IMAGE).await?;
    let options = HandshakeOptions {
        expected_generation: None,
        maximum_reply_bytes: DEFAULT_REPLY_BYTES,
        capabilities: Capabilities::Offer(LIVE.iter().map(|name| (*name).to_owned()).collect()),
    };
    // A disposable host; each connection still gets its own nonce.
    let mut client_nonce = [nonce; 32];
    client_nonce[..4].copy_from_slice(&std::process::id().to_be_bytes());
    Ok(Connection::open(peer, &binding, client_nonce, &options).await?)
}

#[tokio::test]
#[ignore = "needs an isolated StateEngine host; see the module documentation"]
async fn a_live_host_offers_and_answers_the_live_methods() -> TestResult {
    let mut connection = open(1).await?;
    let offered = connection
        .session()
        .capabilities
        .clone()
        .unwrap_or_default();
    assert!(
        LIVE.iter().all(|name| offered.iter().any(|o| o == name)),
        "{offered:?}"
    );
    // A host without an active external seat for the agent refuses with its
    // own typed error: the request decoded, and nothing was recorded.
    let project = std::env::var("GRAPT_SE_LIVE_PROJECT").ok();
    let activity = connection
        .report_activity("main", project.as_deref(), Activity::Idle)
        .await;
    assert!(
        matches!(activity, Err(IpcError::Refused(_))),
        "{activity:?}"
    );
    let stream = open(2).await?.subscribe(SubscribeRequest {
        project_id: None,
        agent_id: None,
        after_cursor: None,
        filter: vec![EventKind::MailNotice],
        max_batch_bytes: 65_536,
    });
    let mut stream = stream.await?;
    let first = tokio::time::timeout(Duration::from_secs(15), stream.next()).await??;
    assert!(matches!(first, StreamFrame::Heartbeat { .. }));
    assert!(stream.resume_cursor().is_some());
    Ok(())
}

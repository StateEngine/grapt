//! A mock host on a Unix socket pair drives the client through its handshake
//! and requests. The host side is written from the published protocol, not
//! from the client's code, so a shared mistake cannot pass unseen.
#![cfg(target_os = "linux")]
use grapt_stateengine_ipc::wire::StartIdentity;
use grapt_stateengine_ipc::{
    connect, discover, observe_self, read_frame, write_frame, Activity, AgentId, AgentSelector,
    Capabilities, CommitState, Connection, Endpoint, EventCursor, EventKind, HandshakeOptions,
    IdempotencyKey, IpcError, PeerBinding, Principal, ProjectId, PublishTurn, SessionId,
    StreamFrame, SubscribeRequest, TurnModel, TurnRole, VerifiedPeer,
};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tokio::net::{UnixListener, UnixStream};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const SECRET: [u8; 32] = [3; 32];
const SERVER_NONCE: [u8; 32] = [1; 32];
const CLIENT_NONCE: [u8; 32] = [2; 32];
const MAX_IMAGE: u64 = 1 << 32;

/// A host played by this test process: its endpoint names this process, so
/// the client's real server check passes against it.
struct Fixture {
    state: tempfile::TempDir,
    listener: UnixListener,
    myself: PeerBinding,
    principal: Principal,
    endpoint: Endpoint,
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        // A short root keeps the socket path within the kernel's limit
        // however long the runner's temporary directory is.
        let state = tempfile::Builder::new()
            .prefix(&format!("gse-{label}-"))
            .tempdir_in("/tmp")?;
        std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700))?;
        let listener = UnixListener::bind(state.path().join("se.sock"))?;
        let myself = observe_self(MAX_IMAGE)?;
        let principal = myself.principal.clone();
        write_private(
            &state.path().join("endpoint.json"),
            &endpoint_json(state.path(), &myself, 0),
        )?;
        write_private(&state.path().join("credential"), &SECRET)?;
        Ok(Self {
            endpoint: discover(state.path(), &principal)?,
            state,
            listener,
            myself,
            principal,
        })
    }

    /// A verified connection to this host and the host's end of it.
    async fn pair(&self) -> TestResult<(VerifiedPeer<UnixStream>, UnixStream)> {
        let peer = connect(
            &self.endpoint,
            &self.principal,
            &self.myself.image_sha256,
            MAX_IMAGE,
        )
        .await?;
        let (host, _) = self.listener.accept().await?;
        Ok((peer, host))
    }

    async fn open(
        &self,
        peer: VerifiedPeer<UnixStream>,
        options: HandshakeOptions,
    ) -> Result<Connection<UnixStream>, IpcError> {
        Connection::open(peer, &self.myself, CLIENT_NONCE, &options).await
    }

    fn epoch(&self) -> Value {
        epoch(&self.myself, 2, 0)
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> TestResult {
    std::fs::write(path, bytes)?;
    Ok(std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(0o600),
    )?)
}

/// This process's executable path, as the kernel reports it.
fn executable() -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    std::fs::read_link("/proc/self/exe")
        .map(|path| path.as_os_str().as_bytes().to_vec())
        .unwrap_or_default()
}

fn start_ticks(binding: &PeerBinding) -> u64 {
    match binding.started {
        StartIdentity::Linux { start_ticks } => start_ticks,
        _ => 0,
    }
}

/// Keys in sorted order at every level, so this is also its canonical form.
/// `skew` moves the process id and start time off this process's own.
fn epoch(host: &PeerBinding, generation: u64, skew: u64) -> Value {
    json!({"generation": generation, "owner": {
        "argv_sha256": "aa".repeat(32), "boot": "boot-fixture",
        "executable": {"encoding": "unix_bytes", "units": executable()},
        "machine": "machine-fixture", "pid": u64::from(host.pid) + skew,
        "started": {"os": "linux", "start_ticks": start_ticks(host) + skew}}})
}

fn endpoint_json(state: &Path, host: &PeerBinding, skew: u64) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let socket = state.join("se.sock");
    json!({"schema": 1, "address": {"kind": "unix_socket", "path": {"encoding": "unix_bytes",
        "units": socket.as_os_str().as_bytes()}}, "epoch": epoch(host, 2, skew), "generation": 2,
        "kind": "standalone", "listener_port": null, "api": {"major": 1, "minor": 0},
        "package_version": "0.1.3", "image_sha256": host.image_sha256.to_string()})
    .to_string()
    .into_bytes()
}

fn options(capabilities: Capabilities, expected_generation: Option<u64>) -> HandshakeOptions {
    HandshakeOptions {
        expected_generation,
        maximum_reply_bytes: grapt_stateengine_ipc::DEFAULT_REPLY_BYTES,
        capabilities,
    }
}

fn challenge(epoch: Value, image: &str) -> Value {
    json!({"version": {"major": 1, "minor": 0}, "epoch": epoch, "nonce": SERVER_NONCE, "image_sha256": image})
}

fn refusal(code: &str) -> Value {
    json!({"error": {"schema": "se.error/v1", "code": code, "detail": {}, "retry": {"kind": "never"},
        "class": "unauthorized", "exit": 1, "message": "refused by the fixture host",
        "remedy": {"text": "fixture remedy", "command": null}}})
}

/// The owner proof as a host computes it from the published field list.
fn expected_mac(fixture: &Fixture, secret: &[u8; 32]) -> TestResult<Vec<u8>> {
    fn field(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(bytes);
    }
    let client = &fixture.myself;
    let Principal::Unix(uid) = client.principal else {
        return Err("not a Unix principal".into());
    };
    let mut peer = b"se.native.peer/v1".to_vec();
    for part in [
        &b"unix"[..],
        &uid.to_be_bytes(),
        &client.pid.to_be_bytes(),
        b"linux",
        &start_ticks(client).to_be_bytes(),
        &client.image_sha256.0,
    ] {
        field(&mut peer, part);
    }
    let mut message = Vec::new();
    let image = fixture.endpoint.file().image_sha256.0;
    for part in [
        &b"se.ipc.owner/v1"[..],
        &[0, 1, 0, 0],
        fixture.epoch().to_string().as_bytes(),
        &image,
        &SERVER_NONCE,
        &CLIENT_NONCE,
        &peer,
    ] {
        field(&mut message, part);
    }
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret);
    Ok(ring::hmac::sign(&key, &message).as_ref().to_vec())
}

async fn send(host: &mut UnixStream, value: &Value) -> TestResult {
    Ok(write_frame(
        host,
        value.to_string().as_bytes(),
        grapt_stateengine_ipc::MAX_FRAME_BYTES,
    )
    .await?)
}

async fn receive(host: &mut UnixStream) -> TestResult<Value> {
    Ok(serde_json::from_slice(
        &read_frame(host, grapt_stateengine_ipc::MAX_FRAME_BYTES).await?,
    )?)
}

/// Challenge, then check the client's hello and proof: the hello on success,
/// or `None` after refusing a wrong proof.
async fn challenge_and_check(
    fixture: &Fixture,
    host: &mut UnixStream,
    secret: &[u8; 32],
) -> TestResult<Option<Value>> {
    let image = fixture.myself.image_sha256.to_string();
    send(host, &challenge(fixture.epoch(), &image)).await?;
    let hello = receive(host).await?;
    let mac: Vec<u8> = serde_json::from_value(hello["authority"]["proof"]["mac"].clone())?;
    if hello["authority"]["kind"] != "owner_proof" || mac != expected_mac(fixture, secret)? {
        send(host, &refusal("owner-proof-invalid")).await?;
        return Ok(None);
    }
    Ok(Some(hello))
}

fn server_hello(fixture: &Fixture, extra: Value) -> Value {
    let mut hello = json!({"version": {"major": 1, "minor": 0}, "epoch": fixture.epoch(), "generation": 2, "maximum_reply_bytes": 4096});
    if let (Some(hello), Some(extra)) = (hello.as_object_mut(), extra.as_object()) {
        hello.extend(extra.clone());
    }
    hello
}

async fn answer(host: &mut UnixStream, generation: u64) -> TestResult<Value> {
    let request = receive(host).await?;
    let output = json!({"state": "output", "value": {"schema": "se.fixture/v1", "value": {"ok": true}, "window": null}});
    send(host, &json!({"version": {"major": 1, "minor": 0}, "request_id": request["request_id"], "generation": generation, "result": output})).await?;
    Ok(request)
}

#[tokio::test]
async fn a_capable_host_negotiates_and_answers_queries_and_keyed_mutations() -> TestResult {
    let fixture = Fixture::new("capable")?;
    let (peer, mut host) = fixture.pair().await?;
    let host_side = async {
        let hello = challenge_and_check(&fixture, &mut host, &SECRET)
            .await?
            .ok_or("proof refused")?;
        assert_eq!(hello["client_capabilities"], json!(["ipc.capabilities"]));
        let metadata = json!({"package_version": "0.1.3", "store_id": "store_fixture", "capabilities": ["ipc.capabilities"]});
        send(&mut host, &server_hello(&fixture, metadata)).await?;
        let query = answer(&mut host, 2).await?;
        let mutation = answer(&mut host, 2).await?;
        TestResult::Ok((query, mutation))
    };
    let client_side = async {
        let offer = Capabilities::Offer(vec!["ipc.capabilities".into()]);
        let mut connection = fixture.open(peer, options(offer, Some(2))).await?;
        assert_eq!(
            connection.session().store_id.as_deref(),
            Some("store_fixture")
        );
        assert_eq!(
            connection.query("project.status", json!({})).await?.value,
            json!({"ok": true})
        );
        let key = IdempotencyKey("key-fixture".into());
        connection
            .mutate("message.send", json!({"body": "x"}), &key)
            .await?;
        Ok::<_, IpcError>(())
    };
    let (host_result, client_result) = tokio::join!(host_side, client_side);
    let (query, mutation) = host_result?;
    client_result?;
    assert_eq!(
        (&query["request_id"], &query["request"]["input"]["key"]),
        (&json!(1), &Value::Null)
    );
    assert_eq!(
        (
            &mutation["request_id"],
            &mutation["request"]["input"]["key"]
        ),
        (&json!(2), &json!("key-fixture"))
    );
    assert_eq!(mutation["generation"], 2);
    Ok(())
}

#[tokio::test]
async fn an_older_host_gets_no_capability_list_and_sends_no_metadata() -> TestResult {
    let fixture = Fixture::new("older")?;
    for unasked in [json!({}), json!({"capabilities": []})] {
        let (peer, mut host) = fixture.pair().await?;
        let host_side = async {
            let hello = challenge_and_check(&fixture, &mut host, &SECRET)
                .await?
                .ok_or("proof refused")?;
            assert!(hello.get("client_capabilities").is_none());
            send(&mut host, &server_hello(&fixture, unasked.clone())).await
        };
        let (host_result, opened) = tokio::join!(
            host_side,
            fixture.open(peer, options(Capabilities::Omit, None))
        );
        host_result?;
        if unasked == json!({}) {
            assert_eq!(opened?.session().capabilities, None);
        } else {
            assert!(matches!(opened, Err(IpcError::HelloMismatch(_))));
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_server_that_is_not_the_endpoints_host_gets_no_proof() -> TestResult {
    let mut fixture = Fixture::new("impostor")?;
    let other_pin = grapt_stateengine_ipc::Sha256Hex([0; 32]);
    let pinned = connect(&fixture.endpoint, &fixture.principal, &other_pin, MAX_IMAGE).await;
    assert!(matches!(pinned, Err(IpcError::BinaryMismatch)));
    let endpoint = fixture.state.path().join("endpoint.json");
    write_private(
        &endpoint,
        &endpoint_json(fixture.state.path(), &fixture.myself, 1),
    )?;
    fixture.endpoint = discover(fixture.state.path(), &fixture.principal)?;
    let moved = fixture.pair().await;
    let refused = moved
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default();
    assert!(refused.contains("is not the endpoint's host"), "{refused}");
    let mut renamed: Value =
        serde_json::from_slice(&endpoint_json(fixture.state.path(), &fixture.myself, 0))?;
    renamed["epoch"]["owner"]["executable"]["units"] = json!(b"/se");
    write_private(&endpoint, renamed.to_string().as_bytes())?;
    fixture.endpoint = discover(fixture.state.path(), &fixture.principal)?;
    let renamed = fixture.pair().await.err().map(|error| error.to_string());
    let renamed = renamed.unwrap_or_default();
    assert!(renamed.contains("another executable"), "{renamed}");
    Ok(())
}

#[tokio::test]
async fn a_challenge_from_another_image_or_epoch_is_refused_before_any_proof() -> TestResult {
    let fixture = Fixture::new("mismatch")?;
    let image = fixture.myself.image_sha256.to_string();
    for sent in [
        challenge(fixture.epoch(), &"cc".repeat(32)),
        challenge(epoch(&fixture.myself, 3, 0), &image),
    ] {
        let (peer, mut host) = fixture.pair().await?;
        send(&mut host, &sent).await?;
        let opened = fixture.open(peer, options(Capabilities::Omit, None)).await;
        assert!(
            matches!(opened, Err(IpcError::ChallengeMismatch(_))),
            "{opened:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn empty_oversized_deep_and_repeated_key_frames_are_refused() -> TestResult {
    let fixture = Fixture::new("frames")?;
    let framed = |bytes: &[u8]| -> TestResult<Vec<u8>> {
        Ok([&u32::try_from(bytes.len())?.to_be_bytes()[..], bytes].concat())
    };
    let deep = "[".repeat(65) + &"]".repeat(65);
    let frames = [
        0_u32.to_be_bytes().to_vec(),
        (grapt_stateengine_ipc::MAX_ENDPOINT_BYTES + 1)
            .to_be_bytes()
            .to_vec(),
        framed(deep.as_bytes())?,
        framed(br#"{"a":1,"a":2}"#)?,
    ];
    for (index, frame) in frames.iter().enumerate() {
        let (peer, mut host) = fixture.pair().await?;
        tokio::io::AsyncWriteExt::write_all(&mut host, frame).await?;
        let opened = fixture.open(peer, options(Capabilities::Omit, None)).await;
        let expected = matches!(
            (index, &opened),
            (0, Err(IpcError::EmptyFrame))
                | (1, Err(IpcError::FrameTooLarge { .. }))
                | (2, Err(IpcError::TooDeep))
                | (3, Err(IpcError::DuplicateKey(_)))
        );
        assert!(expected, "frame {index}: {opened:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_wrong_owner_proof_and_a_stale_generation_are_refused_by_the_host() -> TestResult {
    let fixture = Fixture::new("refusals")?;
    let (peer, mut host) = fixture.pair().await?;
    let (host_result, opened) = tokio::join!(
        challenge_and_check(&fixture, &mut host, &[9; 32]),
        fixture.open(peer, options(Capabilities::Omit, None))
    );
    assert!(host_result?.is_none());
    assert!(matches!(opened, Err(IpcError::Refused(error)) if error.code == "owner-proof-invalid"));
    let (peer, mut host) = fixture.pair().await?;
    let host_side = async {
        let hello = challenge_and_check(&fixture, &mut host, &SECRET)
            .await?
            .ok_or("proof refused")?;
        assert_eq!(hello["expected_generation"], 1);
        send(&mut host, &refusal("stale-generation")).await
    };
    let (host_result, opened) = tokio::join!(
        host_side,
        fixture.open(peer, options(Capabilities::Omit, Some(1)))
    );
    host_result?;
    assert!(matches!(opened, Err(IpcError::Refused(error)) if error.code == "stale-generation"));
    Ok(())
}

#[tokio::test]
async fn an_answer_from_another_generation_breaks_the_connection() -> TestResult {
    let fixture = Fixture::new("generation")?;
    let (peer, mut host) = fixture.pair().await?;
    let host_side = async {
        challenge_and_check(&fixture, &mut host, &SECRET)
            .await?
            .ok_or("proof refused")?;
        send(&mut host, &server_hello(&fixture, json!({}))).await?;
        answer(&mut host, 3).await
    };
    let client_side = async {
        let mut connection = fixture
            .open(peer, options(Capabilities::Omit, None))
            .await?;
        let first = connection.query("project.status", json!({})).await;
        let second = connection.query("project.status", json!({})).await;
        Ok::<_, IpcError>((first, second))
    };
    let (host_result, client_result) = tokio::join!(host_side, client_side);
    host_result?;
    let (first, second) = client_result?;
    assert!(matches!(
        first,
        Err(IpcError::ResponseMismatch("host generation"))
    ));
    assert!(matches!(second, Err(IpcError::ConnectionBroken)));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_silent_host_times_out_the_handshake() -> TestResult {
    let fixture = Fixture::new("silent")?;
    let (peer, _host) = fixture.pair().await?;
    let opened = fixture.open(peer, options(Capabilities::Omit, None)).await;
    assert!(matches!(opened, Err(IpcError::Timeout(_))));
    Ok(())
}

#[tokio::test]
async fn an_endpoint_outside_its_rules_is_refused() -> TestResult {
    let fixture = Fixture::new("endpoint")?;
    let (state, path) = (
        fixture.state.path(),
        fixture.state.path().join("endpoint.json"),
    );
    let valid: Value = serde_json::from_slice(&endpoint_json(state, &fixture.myself, 0))?;
    let mut extra = valid.clone();
    extra["unknown"] = json!(1);
    let mut outside = valid.clone();
    outside["address"]["path"]["units"] = json!(b"/tmp/se.sock");
    let mut not_socket = valid.clone();
    not_socket["address"]["path"]["units"] =
        json!(state.join("credential").as_os_str().as_encoded_bytes());
    for body in [
        extra.to_string().into_bytes(),
        outside.to_string().into_bytes(),
        not_socket.to_string().into_bytes(),
        vec![b' '; 16_385],
    ] {
        write_private(&path, &body)?;
        assert!(discover(state, &fixture.principal).is_err());
    }
    write_private(&path, valid.to_string().as_bytes())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
    assert!(discover(state, &fixture.principal).is_err());
    std::fs::remove_file(&path)?;
    std::os::unix::fs::symlink(state.join("credential"), &path)?;
    assert!(discover(state, &fixture.principal).is_err());
    Ok(())
}

const LIVE: [&str; 4] = [
    "ipc.capabilities",
    "events.subscribe",
    "session.publish_turn",
    "session.activity",
];

/// A verified, negotiated connection to a host that offers `offered`, and
/// the host's end after its hello.
async fn negotiated(
    fixture: &Fixture,
    offered: &[&str],
) -> TestResult<(Connection<UnixStream>, UnixStream)> {
    let (peer, mut host) = fixture.pair().await?;
    let metadata =
        json!({"package_version": "0.1.3", "store_id": "store_fixture", "capabilities": offered});
    let host_side = async {
        challenge_and_check(fixture, &mut host, &SECRET)
            .await?
            .ok_or("proof refused")?;
        send(&mut host, &server_hello(fixture, metadata)).await
    };
    let offer = Capabilities::Offer(LIVE.iter().map(|name| (*name).to_owned()).collect());
    let (host_result, opened) = tokio::join!(host_side, fixture.open(peer, options(offer, None)));
    host_result?;
    Ok((opened?, host))
}

fn output(request: &Value, schema: &str, value: Value) -> Value {
    json!({"version": {"major": 1, "minor": 0}, "request_id": request["request_id"], "generation": 2,
        "result": {"state": "output", "value": {"schema": schema, "value": value, "window": null}}})
}

fn turn(key: &str) -> TestResult<PublishTurn> {
    Ok(PublishTurn {
        idempotency_key: IdempotencyKey(key.into()),
        target_agent_id: Some(AgentId("agent-fixture".into())),
        session_id: Some(SessionId("ses_fixture".into())),
        conversation_id: "conversation-fixture".into(),
        harness: "harness-fixture".into(),
        model: TurnModel {
            provider: "provider-fixture".into(),
            model: "model-fixture".into(),
        },
        vendor_turn_id: "turn-1".into(),
        occurred_at_us: 1,
        role: TurnRole::Assistant,
        body_text: Some("done".into()),
        body_omitted_bytes: 0,
        content_sha256: grapt_stateengine_ipc::Sha256Hex([7; 32]),
        usage: None,
        diff_summary: None,
        decision: None,
        nesting: None,
    })
}

#[tokio::test]
async fn a_host_without_a_capability_never_receives_its_request() -> TestResult {
    let fixture = Fixture::new("unoffered")?;
    let (mut connection, mut host) = negotiated(&fixture, &["ipc.capabilities"]).await?;
    let refused = connection.publish_turn(&project(), &turn("key-a")?).await;
    assert!(matches!(
        refused,
        Err(IpcError::Unsupported("session.publish_turn"))
    ));
    let refused = connection
        .report_activity(&selector(), &project(), Activity::Busy)
        .await;
    assert!(matches!(
        refused,
        Err(IpcError::Unsupported("session.activity"))
    ));
    // Nothing reached the host: its first request is still request 1.
    let (asked, queried) = tokio::join!(
        answer(&mut host, 2),
        connection.query("project.status", json!({}))
    );
    queried?;
    assert_eq!(asked?["request_id"], 1);
    let refused = connection.subscribe(subscribed_request()).await;
    assert!(matches!(
        refused,
        Err(IpcError::Unsupported("events.subscribe"))
    ));
    Ok(())
}

#[tokio::test]
async fn turns_and_activity_carry_their_keys_and_typed_receipts() -> TestResult {
    let fixture = Fixture::new("publish")?;
    let (mut connection, mut host) = negotiated(&fixture, &LIVE).await?;
    let host_side = async {
        let published = receive(&mut host).await?;
        let receipt = json!({"schema": "se.publish-turn-receipt/v1", "agent_id": "agent-fixture",
            "session_id": "ses_fixture", "vendor_turn_id": "turn-1", "cursor": 9, "body_stored": true,
            "content_sha256_stored": true, "content_verified": true, "maintenance_pending": false,
            "commit": {"state": "replayed", "event_id": "ev_fixture", "recorded_at": "t",
                "idempotency_key": "key-a", "writer": {}}});
        send(
            &mut host,
            &output(&published, "se.publish-turn-receipt/v1", receipt),
        )
        .await?;
        let reported = receive(&mut host).await?;
        let state = json!({"project_id": "prj_fixture", "agent_id": "agent-fixture",
            "session_id": "ses_fixture", "state": "idle", "turn_ended": true});
        send(
            &mut host,
            &output(&reported, "se.session-activity/v1", state),
        )
        .await?;
        TestResult::Ok((published, reported))
    };
    let client_side = async {
        let receipt = connection.publish_turn(&project(), &turn("key-a")?).await?;
        let activity = connection
            .report_activity(&selector(), &project(), Activity::Idle)
            .await?;
        TestResult::Ok((receipt, activity))
    };
    let (host_result, client_result) = tokio::join!(host_side, client_side);
    let (published, reported) = host_result?;
    let (receipt, activity) = client_result?;
    let input = &published["request"]["input"];
    assert_eq!(input["name"], "session.publish_turn");
    // Owner authority starts at the registry: the project routes the turn.
    assert_eq!(
        published["cli_selections"],
        json!({"project": "prj_fixture"})
    );
    assert_eq!(
        (&input["key"], &input["arguments"]["idempotency_key"]),
        (&json!("key-a"), &json!("key-a"))
    );
    assert_eq!(input["arguments"]["content_sha256"], "07".repeat(32));
    assert_eq!(
        (receipt.cursor, receipt.commit.state),
        (9, CommitState::Replayed)
    );
    assert_eq!(
        reported["request"],
        json!({"kind": "external", "input": {"op": "session_activity", "project": "prj_fixture",
            "agent": "agent-fixture", "state": "idle"}})
    );
    assert!(activity.turn_ended && activity.state == Activity::Idle);
    Ok(())
}

#[tokio::test]
async fn a_subscription_asks_for_its_filter_and_reads_the_first_frame() -> TestResult {
    let fixture = Fixture::new("subscribe")?;
    let (connection, mut host) = negotiated(&fixture, &LIVE).await?;
    let after = EventCursor::parse("evcur_v1_a").ok_or("cursor")?;
    let request = SubscribeRequest {
        project_id: Some(project()),
        after_cursor: Some(after.clone()),
        filter: vec![EventKind::MailNotice, EventKind::AgentState],
        ..subscribed_request()
    };
    let (subscribed, stream) = tokio::join!(receive(&mut host), connection.subscribe(request));
    let (subscribed, mut stream) = (subscribed?, stream?);
    let input = &subscribed["request"]["input"];
    assert_eq!(subscribed["request"]["kind"], "subscribe");
    assert_eq!(
        (&input["filter"], &input["after_cursor"]),
        (&json!(["mail.notice", "agent.state"]), &json!("evcur_v1_a"))
    );
    assert_eq!(stream.resume_cursor(), Some(&after));
    let heartbeat = json!({"kind": "heartbeat", "head_cursor": "evcur_v1_b"});
    send(
        &mut host,
        &output(&subscribed, "se.event-stream/v1", heartbeat),
    )
    .await?;
    assert!(matches!(
        stream.next().await?,
        StreamFrame::Heartbeat { .. }
    ));
    let (connection, _host) = negotiated(&fixture, &LIVE).await?;
    let small = SubscribeRequest {
        max_batch_bytes: 4095,
        ..subscribed_request()
    };
    assert!(matches!(
        connection.subscribe(small).await,
        Err(IpcError::Invalid(_))
    ));
    Ok(())
}

fn project() -> ProjectId {
    ProjectId("prj_fixture".into())
}

fn selector() -> AgentSelector {
    AgentSelector::Id(AgentId("agent-fixture".into()))
}

fn subscribed_request() -> SubscribeRequest {
    SubscribeRequest {
        project_id: None,
        agent_id: None,
        after_cursor: None,
        filter: vec![EventKind::MailNotice],
        max_batch_bytes: 65_536,
    }
}

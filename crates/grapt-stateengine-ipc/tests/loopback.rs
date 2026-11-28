//! A mock host on a Unix socket pair drives the client through its handshake
//! and requests. The host side is written from the published protocol, not
//! from the client's code, so a shared mistake cannot pass unseen.
#![cfg(unix)]
use grapt_stateengine_ipc::wire::StartIdentity;
use grapt_stateengine_ipc::{
    read_endpoint, read_frame, write_frame, Capabilities, Connection, Endpoint, HandshakeOptions,
    IdempotencyKey, IpcError, OwnerCredential, PeerBinding, Principal, Sha256Hex,
};
use serde_json::{json, Value};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tokio::net::UnixStream;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const SECRET: [u8; 32] = [3; 32];
const SERVER_NONCE: [u8; 32] = [1; 32];
const CLIENT_NONCE: [u8; 32] = [2; 32];
const IMAGE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A private state directory this test created, removed on drop.
struct State(PathBuf);

impl Drop for State {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    state: State,
    principal: Principal,
    endpoint: Endpoint,
    credential: OwnerCredential,
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let path = std::env::temp_dir().join(format!("grapt-ipc-{label}-{}", std::process::id()));
        std::fs::create_dir(&path)?;
        let state = State(path);
        std::fs::set_permissions(&state.0, std::fs::Permissions::from_mode(0o700))?;
        let principal = Principal::Unix(std::fs::metadata(&state.0)?.uid());
        write_private(&state.0.join("endpoint.json"), &endpoint_json(&state.0))?;
        write_private(&state.0.join("credential"), &SECRET)?;
        Ok(Self {
            endpoint: read_endpoint(&state.0, &principal)?,
            credential: OwnerCredential::read(&state.0, &principal)?,
            state,
            principal,
        })
    }

    async fn open(
        &self,
        client: UnixStream,
        options: HandshakeOptions,
    ) -> Result<Connection<UnixStream>, IpcError> {
        Connection::open(
            client,
            &self.endpoint,
            &self.credential,
            &binding(),
            CLIENT_NONCE,
            &options,
        )
        .await
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> TestResult {
    std::fs::write(path, bytes)?;
    Ok(std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(0o600),
    )?)
}

/// Keys in sorted order at every level, so this is also its canonical form.
fn epoch(generation: u64) -> Value {
    json!({"generation": generation, "owner": {
        "argv_sha256": "aa".repeat(32), "boot": "boot-fixture",
        "executable": {"encoding": "unix_bytes", "units": [47, 115, 101]},
        "machine": "machine-fixture", "pid": 12345,
        "started": {"os": "linux", "start_ticks": 987654321}}})
}

fn endpoint_json(state: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let socket = state.join("se.sock");
    json!({"schema": 1, "address": {"kind": "unix_socket", "path": {"encoding": "unix_bytes",
        "units": socket.as_os_str().as_bytes()}}, "epoch": epoch(2), "generation": 2,
        "kind": "standalone", "listener_port": null, "api": {"major": 1, "minor": 0},
        "package_version": "0.1.3", "image_sha256": IMAGE})
    .to_string()
    .into_bytes()
}

fn binding() -> PeerBinding {
    PeerBinding {
        principal: Principal::Unix(1000),
        pid: 4242,
        started: StartIdentity::Linux { start_ticks: 5 },
        image_sha256: Sha256Hex([7; 32]),
    }
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
fn expected_mac(secret: &[u8; 32]) -> Vec<u8> {
    fn field(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(bytes);
    }
    let mut peer = b"se.native.peer/v1".to_vec();
    for part in [
        &b"unix"[..],
        &1000_u32.to_be_bytes(),
        &4242_u32.to_be_bytes(),
        b"linux",
        &5_u64.to_be_bytes(),
        &[7; 32],
    ] {
        field(&mut peer, part);
    }
    let mut message = Vec::new();
    let image = Sha256Hex::parse(IMAGE)
        .map(|digest| digest.0)
        .unwrap_or_default();
    for part in [
        &b"se.ipc.owner/v1"[..],
        &[0, 1, 0, 0],
        epoch(2).to_string().as_bytes(),
        &image,
        &SERVER_NONCE,
        &CLIENT_NONCE,
        &peer,
    ] {
        field(&mut message, part);
    }
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret);
    ring::hmac::sign(&key, &message).as_ref().to_vec()
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
    host: &mut UnixStream,
    secret: &[u8; 32],
) -> TestResult<Option<Value>> {
    send(host, &challenge(epoch(2), IMAGE)).await?;
    let hello = receive(host).await?;
    let mac: Vec<u8> = serde_json::from_value(hello["authority"]["proof"]["mac"].clone())?;
    if hello["authority"]["kind"] != "owner_proof" || mac != expected_mac(secret) {
        send(host, &refusal("owner-proof-invalid")).await?;
        return Ok(None);
    }
    Ok(Some(hello))
}

fn server_hello(extra: Value) -> Value {
    let mut hello = json!({"version": {"major": 1, "minor": 0}, "epoch": epoch(2), "generation": 2, "maximum_reply_bytes": 4096});
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
    let (client, mut host) = UnixStream::pair()?;
    let host_side = async {
        let hello = challenge_and_check(&mut host, &SECRET)
            .await?
            .ok_or("proof refused")?;
        assert_eq!(hello["client_capabilities"], json!(["ipc.capabilities"]));
        send(&mut host, &server_hello(json!({"package_version": "0.1.3", "store_id": "store_fixture", "capabilities": ["ipc.capabilities"]}))).await?;
        let query = answer(&mut host, 2).await?;
        let mutation = answer(&mut host, 2).await?;
        TestResult::Ok((query, mutation))
    };
    let client_side = async {
        let mut connection = fixture
            .open(
                client,
                options(
                    Capabilities::Offer(vec!["ipc.capabilities".into()]),
                    Some(2),
                ),
            )
            .await?;
        assert_eq!(
            connection.session().store_id.as_deref(),
            Some("store_fixture")
        );
        assert_eq!(
            connection.query("project.status", json!({})).await?.value,
            json!({"ok": true})
        );
        connection
            .mutate(
                "message.send",
                json!({"body": "x"}),
                &IdempotencyKey("key-fixture".into()),
            )
            .await?;
        Ok::<_, IpcError>(())
    };
    let (host_result, client_result) = tokio::join!(host_side, client_side);
    let (query, mutation) = host_result?;
    client_result?;
    assert_eq!(
        (
            query["request_id"].clone(),
            query["request"]["input"]["key"].clone()
        ),
        (json!(1), Value::Null)
    );
    assert_eq!(
        (
            mutation["request_id"].clone(),
            mutation["request"]["input"]["key"].clone()
        ),
        (json!(2), json!("key-fixture"))
    );
    assert_eq!(mutation["generation"], 2);
    Ok(())
}

#[tokio::test]
async fn an_older_host_gets_no_capability_list_and_sends_no_metadata() -> TestResult {
    let fixture = Fixture::new("older")?;
    for unasked in [json!({}), json!({"capabilities": []})] {
        let (client, mut host) = UnixStream::pair()?;
        let host_side = async {
            let hello = challenge_and_check(&mut host, &SECRET)
                .await?
                .ok_or("proof refused")?;
            assert!(hello.get("client_capabilities").is_none());
            send(&mut host, &server_hello(unasked.clone())).await
        };
        let (host_result, opened) = tokio::join!(
            host_side,
            fixture.open(client, options(Capabilities::Omit, None))
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
async fn a_challenge_from_another_image_or_epoch_is_refused_before_any_proof() -> TestResult {
    let fixture = Fixture::new("mismatch")?;
    for sent in [
        challenge(epoch(2), &"cc".repeat(32)),
        challenge(epoch(3), IMAGE),
    ] {
        let (client, mut host) = UnixStream::pair()?;
        send(&mut host, &sent).await?;
        let opened = fixture
            .open(client, options(Capabilities::Omit, None))
            .await;
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
    let deep = "[".repeat(65) + &"]".repeat(65);
    let framed = |bytes: &[u8]| -> TestResult<Vec<u8>> {
        Ok([&u32::try_from(bytes.len())?.to_be_bytes()[..], bytes].concat())
    };
    let frames = [
        0_u32.to_be_bytes().to_vec(),
        (grapt_stateengine_ipc::MAX_ENDPOINT_BYTES + 1)
            .to_be_bytes()
            .to_vec(),
        framed(deep.as_bytes())?,
        framed(br#"{"a":1,"a":2}"#)?,
    ];
    for (index, frame) in frames.iter().enumerate() {
        let (client, mut host) = UnixStream::pair()?;
        tokio::io::AsyncWriteExt::write_all(&mut host, frame).await?;
        let opened = fixture
            .open(client, options(Capabilities::Omit, None))
            .await;
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
    let (client, mut host) = UnixStream::pair()?;
    let (host_result, opened) = tokio::join!(
        challenge_and_check(&mut host, &[9; 32]),
        fixture.open(client, options(Capabilities::Omit, None))
    );
    assert!(host_result?.is_none());
    assert!(matches!(opened, Err(IpcError::Refused(error)) if error.code == "owner-proof-invalid"));
    let (client, mut host) = UnixStream::pair()?;
    let host_side = async {
        let hello = challenge_and_check(&mut host, &SECRET)
            .await?
            .ok_or("proof refused")?;
        assert_eq!(hello["expected_generation"], 1);
        send(&mut host, &refusal("stale-generation")).await
    };
    let (host_result, opened) = tokio::join!(
        host_side,
        fixture.open(client, options(Capabilities::Omit, Some(1)))
    );
    host_result?;
    assert!(matches!(opened, Err(IpcError::Refused(error)) if error.code == "stale-generation"));
    Ok(())
}

#[tokio::test]
async fn an_answer_from_another_generation_breaks_the_connection() -> TestResult {
    let fixture = Fixture::new("generation")?;
    let (client, mut host) = UnixStream::pair()?;
    let host_side = async {
        challenge_and_check(&mut host, &SECRET)
            .await?
            .ok_or("proof refused")?;
        send(&mut host, &server_hello(json!({}))).await?;
        answer(&mut host, 3).await
    };
    let client_side = async {
        let mut connection = fixture
            .open(client, options(Capabilities::Omit, None))
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
    let (client, _host) = UnixStream::pair()?;
    let opened = fixture
        .open(client, options(Capabilities::Omit, None))
        .await;
    assert!(matches!(opened, Err(IpcError::Timeout(_))));
    Ok(())
}

#[test]
fn an_endpoint_outside_its_rules_is_refused() -> TestResult {
    let fixture = Fixture::new("endpoint")?;
    let path = fixture.state.0.join("endpoint.json");
    let mut extra: Value = serde_json::from_slice(&endpoint_json(&fixture.state.0))?;
    extra["unknown"] = json!(1);
    let mut outside: Value = serde_json::from_slice(&endpoint_json(&fixture.state.0))?;
    outside["address"]["path"]["units"] = json!(b"/tmp/se.sock");
    for body in [
        extra.to_string().into_bytes(),
        outside.to_string().into_bytes(),
        vec![b' '; 16_385],
    ] {
        write_private(&path, &body)?;
        assert!(read_endpoint(&fixture.state.0, &fixture.principal).is_err());
    }
    write_private(&path, &endpoint_json(&fixture.state.0))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
    assert!(read_endpoint(&fixture.state.0, &fixture.principal).is_err());
    std::fs::remove_file(&path)?;
    std::os::unix::fs::symlink(fixture.state.0.join("credential"), &path)?;
    assert!(read_endpoint(&fixture.state.0, &fixture.principal).is_err());
    Ok(())
}

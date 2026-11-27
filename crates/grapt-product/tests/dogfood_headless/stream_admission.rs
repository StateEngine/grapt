//! Event subscriptions retain capacity without starving ordinary consumers.
use super::*;
use process_fixture::{cleanup, spawn_captured, spawn_controlled};

const PATHS: [&str; 2] = ["/api/event", "/api/capture/v1/events"];

fn subscription(address: SocketAddr, path: &str) -> (TcpStream, String) {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .unwrap_or_else(|error| panic!("subscription connect: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap_or_else(|error| panic!("subscription read allowance: {error}"));
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap_or_else(|error| panic!("subscription request: {error}"));
    // Read the head without consuming any event bytes needed by the reader.
    let mut head = Vec::new();
    while head.len() < 16 * 1024 && !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        stream
            .read_exact(&mut byte)
            .unwrap_or_else(|error| panic!("subscription head: {error}"));
        head.push(byte[0]);
    }
    assert!(
        head.ends_with(b"\r\n\r\n"),
        "subscription head exceeded its bound"
    );
    (
        stream,
        String::from_utf8(head)
            .unwrap_or_else(|error| panic!("subscription head encoding: {error}")),
    )
}

fn status(head: &str) -> u16 {
    head.split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("subscription status absent: {head}"))
}

fn pressure(root: &Path) -> Vec<(usize, usize)> {
    fs::read_to_string(root.join("product-stderr-0.log"))
        .unwrap_or_else(|error| panic!("service diagnostics: {error}"))
        .lines()
        .filter(|line| line.contains("occupied=") && line.contains("capacity="))
        .map(|line| {
            assert_eq!(
                line.split_whitespace().next(),
                Some("warning:"),
                "pressure diagnostic severity: {line}"
            );
            let count = |key: &str| {
                line.split_whitespace()
                    .find_map(|part| part.strip_prefix(key))
                    .and_then(|value| value.trim_end_matches(';').parse().ok())
                    .unwrap_or_else(|| panic!("pressure diagnostic omitted numeric {key}: {line}"))
            };
            (count("occupied="), count("capacity="))
        })
        .collect()
}

fn stream_limit(root: &Path, count: usize) {
    let mut config = fs::OpenOptions::new()
        .append(true)
        .open(root.join("service.conf"))
        .unwrap_or_else(|error| panic!("stream configuration: {error}"));
    writeln!(config, "max_inflight_streams={count}")
        .unwrap_or_else(|error| panic!("stream configuration write: {error}"));
}

fn ordinary_reads(address: SocketAddr, session: &str) {
    for path in [
        "/api/health".to_owned(),
        format!("/api/session/{session}/message?limit=1"),
    ] {
        let (status, bytes) = request(address, "GET", &path, "");
        assert_eq!(
            status,
            200,
            "ordinary {path}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
}

fn refused(head: &str) {
    assert_eq!(status(head), 503, "stream-only overload: {head}");
    let head = head.to_ascii_lowercase();
    assert!(
        head.lines().any(|line| line == "retry-after: 1"),
        "retry advice absent: {head}"
    );
    assert!(
        head.lines().any(|line| line == "connection: close"),
        "refused stream retained its connection: {head}"
    );
}

#[test]
fn full_stream_capacity_preserves_history_and_refuses_only_more_subscriptions() {
    let _journey = process_journey_custody();
    let root = root("stream-capacity");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let mut streams = Vec::new();
    let mut overload = None;
    // Discover the actual capacity before checking ordinary traffic. On the
    // old shared budget this fills all ordinary slots, rather than stopping
    // early because the new stream default is smaller.
    for index in 0..40 {
        let (stream, head) = subscription(address, PATHS[index % PATHS.len()]);
        match status(&head) {
            200 => streams.push(stream),
            503 => {
                overload = Some(head);
                break;
            }
            other => panic!("subscription returned {other}: {head}"),
        }
    }
    assert!(
        overload.is_some(),
        "subscriptions never reached a finite capacity"
    );
    ordinary_reads(address, &session);
    assert_eq!(streams.len(), 16, "default event stream capacity");
    refused(
        overload
            .as_deref()
            .unwrap_or_else(|| panic!("no stream refusal")),
    );
    for path in PATHS {
        let (_stream, head) = subscription(address, path);
        refused(&head);
    }
    drop(streams);
    child.stop_after_witness(&root);
    cleanup(&root);
}

#[test]
fn stream_pressure_is_quiet_at_three_quarters_and_warns_above_it() {
    let _journey = process_journey_custody();
    let root = root("stream-pressure");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let mut streams = Vec::new();
    for index in 0..16 {
        let (stream, head) = subscription(address, PATHS[index % PATHS.len()]);
        assert_eq!(
            status(&head),
            200,
            "admitted subscription {}: {head}",
            index + 1
        );
        streams.push(stream);
        if index < 12 {
            assert!(
                pressure(&root).is_empty(),
                "75% occupancy must remain quiet"
            );
        } else {
            let expected = (index + 1, 16);
            assert!(
                pressure(&root).contains(&expected),
                "missing stream pressure diagnostic: {expected:?}"
            );
        }
    }
    ordinary_reads(address, &session);
    let warnings = pressure(&root);
    for path in PATHS {
        let (_stream, head) = subscription(address, path);
        refused(&head);
    }
    assert_eq!(
        pressure(&root),
        warnings,
        "ordinary traffic or refused streams emitted pressure warnings"
    );
    drop(streams);
    child.stop_after_witness(&root);
    cleanup(&root);
}

#[test]
fn configured_stream_capacity_and_zero_leave_ordinary_requests_available() {
    let _journey = process_journey_custody();
    for limit in [2, 0] {
        let root = root(&format!("configured-stream-capacity-{limit}"));
        prepare(&root, 120_000);
        stream_limit(&root, limit);
        let provider = ProviderFixture::start(Vec::new());
        let mut child = spawn_controlled(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        let streams: Vec<_> = (0..limit)
            .map(|index| {
                let (stream, head) = subscription(address, PATHS[index % PATHS.len()]);
                assert_eq!(status(&head), 200, "configured stream: {head}");
                stream
            })
            .collect();
        for path in PATHS {
            let (_stream, head) = subscription(address, path);
            refused(&head);
        }
        ordinary_reads(address, &session);
        drop(streams);
        child.stop_after_witness(&root);
        cleanup(&root);
    }
}

#[test]
fn killing_an_owned_subscriber_promptly_reclaims_its_only_slot() {
    const CLIENT_ROOT: &str = "GRAPT_STREAM_CLIENT_ROOT";
    const CLIENT_ADDRESS: &str = "GRAPT_STREAM_CLIENT_ADDRESS";
    if let Some(root) = std::env::var_os(CLIENT_ROOT) {
        let address = std::env::var(CLIENT_ADDRESS)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("owned subscriber address absent"));
        let (mut stream, head) = subscription(address, PATHS[0]);
        assert_eq!(status(&head), 200, "owned subscriber was refused: {head}");
        stream
            .set_nonblocking(true)
            .unwrap_or_else(|error| panic!("owned subscriber mode: {error}"));
        fs::write(PathBuf::from(root).join("subscribed"), b"ready")
            .unwrap_or_else(|error| panic!("subscriber readiness: {error}"));
        // The parent kills this owned process; this is only a leak fallback.
        let lifetime = PollDeadline::within(Duration::from_secs(90));
        while lifetime.pending() {
            let mut bytes = [0u8; 8192];
            match stream.read(&mut bytes) {
                Ok(0) => panic!("owned subscriber was closed before its process was killed"),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("owned subscriber read: {error}"),
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("owned subscriber outlived its parent test");
    }
    let _journey = process_journey_custody();
    let root = root("killed-stream-client");
    prepare(&root, 120_000);
    stream_limit(&root, 1);
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let client_root = root.join("subscriber");
    fs::create_dir_all(&client_root).unwrap_or_else(|error| panic!("subscriber root: {error}"));
    let mut command = Command::new(
        std::env::current_exe().unwrap_or_else(|error| panic!("test executable: {error}")),
    );
    command
        .arg("--exact")
        .arg("stream_admission::killing_an_owned_subscriber_promptly_reclaims_its_only_slot")
        .arg("--nocapture")
        .env(CLIENT_ROOT, &client_root)
        .env(CLIENT_ADDRESS, address.to_string());
    let mut client = spawn_captured(&client_root, &mut command);
    let ready = PollDeadline::within(Duration::from_secs(10));
    assert!(
        ready
            .poll(Duration::from_millis(10), || client_root
                .join("subscribed")
                .exists()
                .then_some(()))
            .is_some(),
        "owned subscriber never became ready ({ready})"
    );
    let (overflow, head) = subscription(address, PATHS[1]);
    refused(&head);
    drop(overflow);
    client
        .kill()
        .unwrap_or_else(|error| panic!("owned subscriber kill: {error}"));
    client
        .wait()
        .unwrap_or_else(|error| panic!("owned subscriber reap: {error}"));
    let reclaim = PollDeadline::within(Duration::from_secs(2));
    let replacement = reclaim.poll(Duration::from_millis(10), || {
        let (stream, head) = subscription(address, PATHS[1]);
        match status(&head) {
            200 => Some(stream),
            503 => None,
            other => panic!("replacement subscription returned {other}: {head}"),
        }
    });
    assert!(
        replacement.is_some() && reclaim.elapsed() < Duration::from_secs(3),
        "killed subscriber retained its permit ({reclaim})"
    );
    drop(replacement);
    drop(client);
    child.stop_after_witness(&root);
    cleanup(&root);
}

#[test]
fn reading_subscriptions_survive_the_write_allowance_without_application_events() {
    let _journey = process_journey_custody();
    let root = root("healthy-stream-readers");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let mut streams: Vec<_> = PATHS
        .into_iter()
        .map(|path| {
            let (stream, head) = subscription(address, path);
            assert_eq!(status(&head), 200, "healthy reader admission: {head}");
            stream
                .set_nonblocking(true)
                .unwrap_or_else(|error| panic!("reader mode: {error}"));
            stream
        })
        .collect();
    let wait = PollDeadline::within(Duration::from_secs(47));
    let mut last_progress = [Duration::ZERO; 2];
    let mut after_allowance = [false; 2];
    while wait.pending() {
        for (index, stream) in streams.iter_mut().enumerate() {
            let mut bytes = [0u8; 8192];
            match stream.read(&mut bytes) {
                Ok(0) => panic!("healthy {} stream ended after {}", PATHS[index], wait),
                Ok(_) => {
                    last_progress[index] = wait.elapsed();
                    after_allowance[index] |= wait.elapsed() > Duration::from_secs(31);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("healthy {} stream: {error}", PATHS[index]),
            }
            assert!(
                wait.elapsed().saturating_sub(last_progress[index]) < Duration::from_secs(22),
                "idle {} reader received no heartbeat ({wait})",
                PATHS[index]
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        after_allowance.into_iter().all(|progress| progress),
        "a healthy stream made no progress beyond the write allowance"
    );
    assert_eq!(request(address, "GET", "/api/health", "").0, 200);
    drop(streams);
    child.stop_after_witness(&root);
    cleanup(&root);
}

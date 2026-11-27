//! A framing rejection must reach the real Session as a visible terminal.
use super::provider_fixture::read_provider_request;
use super::*;
use std::io::{BufRead, BufReader};

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn rejected_provider_framing_settles_the_session_with_a_visible_failure() {
    let _journey = process_journey_custody();
    let root = root("provider-framing-rejection");
    prepare(&root, 200_000);
    let provider = ProviderFixture::start(Vec::new());
    let fixture_address = provider.address;
    let listener = TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("framing proxy bind: {error}"));
    listener
        .set_nonblocking(true)
        .unwrap_or_else(|error| panic!("framing proxy nonblocking: {error}"));
    let proxy_address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("framing proxy address: {error}"));
    let proxy = thread::spawn(move || {
        let (mut client, _) = PollDeadline::within(client_allowance::CLI_REQUEST_ALLOWANCE)
            .poll(Duration::from_millis(10), || match listener.accept() {
                Ok(value) => Some(value),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
                Err(error) => panic!("framing proxy accept: {error}"),
            })
            .unwrap_or_else(|| panic!("framing proxy received no request"));
        client
            .set_nonblocking(false)
            .unwrap_or_else(|error| panic!("framing proxy blocking: {error}"));
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap_or_else(|error| panic!("framing proxy write allowance: {error}"));
        let (head, body) = read_provider_request(&mut client)
            .unwrap_or_else(|error| panic!("actual product request framing: {error}"));
        assert!(head
            .lines()
            .any(|line| line.to_ascii_lowercase().starts_with("content-length:")));
        // The real product sends a fixed JSON body. Deliberately remove just
        // its framing header to exercise the fixture's observed rejection.
        let malformed = head
            .lines()
            .filter(|line| !line.to_ascii_lowercase().starts_with("content-length:"))
            .collect::<Vec<_>>()
            .join("\r\n");
        let mut upstream = TcpStream::connect_timeout(&fixture_address, Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("framing proxy connect: {error}"));
        upstream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap_or_else(|error| panic!("framing proxy read allowance: {error}"));
        upstream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap_or_else(|error| panic!("framing proxy upstream write allowance: {error}"));
        upstream
            .write_all(format!("{malformed}\r\n\r\n{body}").as_bytes())
            .unwrap_or_else(|error| panic!("framing proxy forward: {error}"));
        let mut response = Vec::new();
        let mut reader = BufReader::new(upstream.take(8192));
        while !response.ends_with(b"\r\n\r\n") {
            let count = reader
                .read_until(b'\n', &mut response)
                .unwrap_or_else(|error| panic!("framing proxy response header: {error}"));
            assert!(
                count > 0,
                "fixture returned no complete bounded HTTP rejection"
            );
        }
        assert!(String::from_utf8_lossy(&response).contains("Content-Length: 0\r\n"));
        assert!(
            response.starts_with(b"HTTP/1.1 400 "),
            "fixture did not return a bounded HTTP rejection"
        );
        client
            .write_all(&response)
            .unwrap_or_else(|error| panic!("framing proxy relay: {error}"));
    });
    let mut child = spawn(&root, proxy_address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (events, connected) = event_stream(address);
    let collector = thread::spawn(move || collect(events, connected, "execution.failed"));
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"show the provider rejection"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    proxy
        .join()
        .unwrap_or_else(|_| panic!("framing proxy panicked"));
    let observed = collector
        .join()
        .unwrap_or_else(|_| panic!("framing failure collector panicked"));
    assert!(
        observed.contains("ExecutionFailed"),
        "framing refusal lost its visible failure: {observed}"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let refused = std::mem::take(&mut *locked(&provider.refused));
    assert_eq!(
        refused.len(),
        1,
        "framing rejection was not recorded exactly once: {refused:?}"
    );
    assert!(
        refused[0].contains("POST /responses HTTP/1.1"),
        "{refused:?}"
    );
    assert!(refused[0].contains("Transfer-Encoding=None"), "{refused:?}");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn an_unexpected_parser_refusal_still_fails_the_fixture() {
    unexpected_parser_refusal(ProviderFixture::start(Vec::new()));
}

#[test]
fn a_cut_tolerant_fixture_still_fails_on_unexpected_parser_refusal() {
    unexpected_parser_refusal(ProviderFixture::start_tolerating_cuts(Vec::new()));
}

#[test]
fn a_cut_request_keeps_the_next_script_and_only_tolerant_mode_accepts_the_cut() {
    for tolerate in [false, true] {
        let scripts = vec![Script::SuccessText("remaining-script-marker")];
        let provider = if tolerate {
            ProviderFixture::start_tolerating_cuts(scripts)
        } else {
            ProviderFixture::start(scripts)
        };
        let exchange = |request: &[u8]| {
            let mut stream = TcpStream::connect_timeout(&provider.address, Duration::from_secs(5))
                .unwrap_or_else(|error| panic!("fixture connect: {error}"));
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap_or_else(|error| panic!("fixture read allowance: {error}"));
            stream
                .write_all(request)
                .unwrap_or_else(|error| panic!("fixture request: {error}"));
            stream
                .shutdown(std::net::Shutdown::Write)
                .unwrap_or_else(|error| panic!("fixture cut: {error}"));
            let mut response = String::new();
            stream
                .take(8192)
                .read_to_string(&mut response)
                .unwrap_or_else(|error| panic!("fixture response: {error}"));
            response
        };
        let cut = exchange(b"POST /responses HTTP/1.1\r\nContent-Length: 8\r\n\r\n{");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(locked(&provider.truncated).len(), usize::from(tolerate));
        let refused = std::mem::take(&mut *locked(&provider.refused));
        assert_eq!(refused.len(), usize::from(!tolerate));
        if tolerate {
            assert!(cut.is_empty(), "a tolerated cut received a refusal");
        } else {
            assert!(cut.starts_with("HTTP/1.1 400 "));
        }
        let completed = exchange(b"POST /responses HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}");
        assert!(completed.contains("remaining-script-marker"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
}

fn unexpected_parser_refusal(provider: ProviderFixture) {
    let mut client = TcpStream::connect_timeout(&provider.address, Duration::from_secs(5))
        .unwrap_or_else(|error| panic!("unexpected refusal connect: {error}"));
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap_or_else(|error| panic!("unexpected refusal read allowance: {error}"));
    client
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap_or_else(|error| panic!("unexpected refusal write allowance: {error}"));
    client
        .write_all(b"POST /responses HTTP/1.1\r\nHost: loopback\r\n\r\n")
        .unwrap_or_else(|error| panic!("unexpected refusal request: {error}"));
    let mut response = Vec::new();
    client
        .take(8192)
        .read_to_end(&mut response)
        .unwrap_or_else(|error| panic!("unexpected refusal response: {error}"));
    assert!(response.starts_with(b"HTTP/1.1 400 "));
    // Drop consumes the fixture and joins its workers before reporting the
    // retained refusal; no fixture state is reused across this boundary.
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(provider)))
        .err()
        .unwrap_or_else(|| panic!("an unconsumed malformed request silently passed"));
    let reason = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .unwrap_or_else(|| panic!("unexpected fixture failure type"));
    assert!(
        reason.contains("provider request omitted Content-Length"),
        "{reason}"
    );
    assert!(reason.contains("POST /responses HTTP/1.1"), "{reason}");
}

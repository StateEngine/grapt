use std::sync::Arc;
use std::time::Duration;

use grapt_service::{
    AuthenticatedHealthProbe, HealthExpectation, HealthProbeRefusal, ProbeOutcome,
};
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceSlotId,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn required<T>(value: Option<T>, label: &str) -> T {
    match value {
        Some(value) => value,
        None => panic!("{label} was refused"),
    }
}

fn owner(endpoint: Endpoint) -> OwnerReceipt {
    OwnerReceipt::new(
        required(ServiceSlotId::new("product"), "slot"),
        required(ServiceInstanceId::new("instance-a"), "instance"),
        required(EnvironmentReceipt::new("internal-alpha"), "environment"),
        ServiceGeneration::FIRST,
        required(ProcessId::new(41), "pid"),
        endpoint,
    )
}

async fn serve_once(body: Arc<str>) -> (Endpoint, tokio::task::JoinHandle<()>) {
    serve_response(body, 200, Duration::ZERO, Duration::ZERO).await
}

async fn serve_response(
    body: Arc<str>,
    status: u16,
    delay: Duration,
    keep_open: Duration,
) -> (Endpoint, tokio::task::JoinHandle<()>) {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) => panic!("listener bind failed: {error}"),
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => panic!("listener address failed: {error}"),
    };
    let server = tokio::spawn(async move {
        let (mut stream, _) = match listener.accept().await {
            Ok(value) => value,
            Err(error) => panic!("accept failed: {error}"),
        };
        let mut request = [0_u8; 2_048];
        let read = match stream.read(&mut request).await {
            Ok(read) => read,
            Err(error) => panic!("request read failed: {error}"),
        };
        assert!(
            request[..read]
                .windows(b"Authorization: Basic YWxwaGE6emVwaHly".len())
                .any(|window| window == b"Authorization: Basic YWxwaGE6emVwaHly"),
            "the self-probe must present the configured Basic credential"
        );
        tokio::time::sleep(delay).await;
        let response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );
        if let Err(error) = stream.write_all(response.as_bytes()).await {
            // A deliberately late response may meet a probe that already timed out.
            assert!(
                !delay.is_zero()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                    ),
                "response write failed: {error}"
            );
        }
        tokio::time::sleep(keep_open).await;
    });
    (
        required(Endpoint::new(format!("http://{address}")), "endpoint"),
        server,
    )
}

async fn join_server(mut server: tokio::task::JoinHandle<()>) {
    match tokio::time::timeout(Duration::from_secs(5), &mut server).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("health fixture server failed: {error}"),
        Err(_) => {
            server.abort();
            let joined = server.await;
            panic!("health fixture server exceeded its join bound: {joined:?}");
        }
    }
}

#[tokio::test]
async fn matching_authenticated_health_is_attributed() {
    let body: Arc<str> = Arc::from(
        r#"{"schema":"secode.api.health.v1","pid":41,"build":"build-a","environment":"internal-alpha","service_instance":"instance-a"}"#,
    );
    let (endpoint, server) = serve_once(body).await;
    let expected_owner = owner(endpoint);
    let expectation = match HealthExpectation::new(expected_owner.clone(), "build-a") {
        Ok(value) => value,
        Err(error) => panic!("expectation failed: {error}"),
    };
    let probe = match AuthenticatedHealthProbe::new("alpha", "zephyr", expectation) {
        Ok(value) => value,
        Err(error) => panic!("probe construction failed: {error}"),
    };
    let outcome = probe.probe().await;
    join_server(server).await;
    assert_eq!(outcome, Ok(ProbeOutcome::Attributed(expected_owner)));
}

#[tokio::test]
async fn mismatched_build_refuses_attribution() {
    let body: Arc<str> = Arc::from(
        r#"{"schema":"secode.api.health.v1","pid":41,"build":"other","environment":"internal-alpha","service_instance":"instance-a"}"#,
    );
    let (endpoint, server) = serve_once(body).await;
    let expectation = match HealthExpectation::new(owner(endpoint), "build-a") {
        Ok(value) => value,
        Err(error) => panic!("expectation failed: {error}"),
    };
    let probe = match AuthenticatedHealthProbe::new("alpha", "zephyr", expectation) {
        Ok(value) => value,
        Err(error) => panic!("probe construction failed: {error}"),
    };
    let outcome = probe.probe().await;
    join_server(server).await;
    assert_eq!(outcome, Err(HealthProbeRefusal::IdentityMismatch));
}

#[tokio::test]
async fn mismatched_environment_refuses_attribution() {
    let body: Arc<str> = Arc::from(
        r#"{"schema":"secode.api.health.v1","pid":41,"build":"build-a","environment":"other","service_instance":"instance-a"}"#,
    );
    let (endpoint, server) = serve_once(body).await;
    let expectation = match HealthExpectation::new(owner(endpoint), "build-a") {
        Ok(value) => value,
        Err(error) => panic!("expectation failed: {error}"),
    };
    let probe = match AuthenticatedHealthProbe::new("alpha", "zephyr", expectation) {
        Ok(value) => value,
        Err(error) => panic!("probe construction failed: {error}"),
    };
    let outcome = probe.probe().await;
    join_server(server).await;
    assert_eq!(outcome, Err(HealthProbeRefusal::IdentityMismatch));
}

#[tokio::test]
async fn mismatched_service_instance_refuses_attribution() {
    let body: Arc<str> = Arc::from(
        r#"{"schema":"secode.api.health.v1","pid":41,"build":"build-a","environment":"internal-alpha","service_instance":"other"}"#,
    );
    let (endpoint, server) = serve_once(body).await;
    let expectation = match HealthExpectation::new(owner(endpoint), "build-a") {
        Ok(value) => value,
        Err(error) => panic!("expectation failed: {error}"),
    };
    let probe = match AuthenticatedHealthProbe::new("alpha", "zephyr", expectation) {
        Ok(value) => value,
        Err(error) => panic!("probe construction failed: {error}"),
    };
    let outcome = probe.probe().await;
    join_server(server).await;
    assert_eq!(outcome, Err(HealthProbeRefusal::IdentityMismatch));
}

#[tokio::test]
async fn mismatched_schema_refuses_attribution() {
    let body: Arc<str> = Arc::from(
        r#"{"schema":"grapt.api.health.v2","pid":41,"build":"build-a","environment":"internal-alpha","service_instance":"instance-a"}"#,
    );
    let (endpoint, server) = serve_once(body).await;
    let expectation = match HealthExpectation::new(owner(endpoint), "build-a") {
        Ok(value) => value,
        Err(error) => panic!("expectation failed: {error}"),
    };
    let probe = match AuthenticatedHealthProbe::new("alpha", "zephyr", expectation) {
        Ok(value) => value,
        Err(error) => panic!("probe construction failed: {error}"),
    };
    let outcome = probe.probe().await;
    join_server(server).await;
    assert_eq!(outcome, Err(HealthProbeRefusal::IdentityMismatch));
}

const MATCHING_HEALTH: &str = r#"{"schema":"secode.api.health.v1","pid":41,"build":"build-a","environment":"internal-alpha","service_instance":"instance-a"}"#;

fn matching_probe(endpoint: Endpoint) -> AuthenticatedHealthProbe {
    let expected = match HealthExpectation::new(owner(endpoint), "build-a") {
        Ok(expected) => expected,
        Err(error) => panic!("expectation failed: {error}"),
    };
    match AuthenticatedHealthProbe::new("alpha", "zephyr", expected) {
        Ok(probe) => probe,
        Err(error) => panic!("probe construction failed: {error}"),
    }
}

#[tokio::test]
async fn a_valid_response_delayed_1200ms_is_attributed() {
    let (endpoint, server) = serve_response(
        Arc::from(MATCHING_HEALTH),
        200,
        Duration::from_millis(1200),
        Duration::ZERO,
    )
    .await;
    let expected_owner = owner(endpoint.clone());
    let outcome = matching_probe(endpoint).probe().await;
    join_server(server).await;
    assert_eq!(outcome, Ok(ProbeOutcome::Attributed(expected_owner)));
}

#[tokio::test]
async fn a_valid_response_delayed_2500ms_remains_a_read_timeout() {
    let (endpoint, server) = serve_response(
        Arc::from(MATCHING_HEALTH),
        200,
        Duration::from_millis(2500),
        Duration::ZERO,
    )
    .await;
    let outcome = matching_probe(endpoint).probe().await;
    join_server(server).await;
    assert_eq!(outcome, Err(HealthProbeRefusal::ReadTimeout));
}

#[tokio::test]
async fn an_oversized_response_refuses_before_the_peer_closes() {
    let (endpoint, server) = serve_response(
        Arc::from("x".repeat(8193)),
        200,
        Duration::ZERO,
        Duration::from_millis(2500),
    )
    .await;
    let outcome = matching_probe(endpoint).probe().await;
    let peer_still_open = !server.is_finished();
    join_server(server).await;
    assert_eq!(outcome, Err(HealthProbeRefusal::ResponseTooLarge));
    assert!(peer_still_open, "oversize refusal waited for peer EOF");
}

#[tokio::test]
async fn status_body_and_pid_refusals_are_preserved() {
    for (body, status, expected) in [
        (
            Arc::from(MATCHING_HEALTH),
            503,
            HealthProbeRefusal::Status(503),
        ),
        (
            Arc::from("x".repeat(4097)),
            200,
            HealthProbeRefusal::BodyTooLarge,
        ),
        (Arc::from("not JSON"), 200, HealthProbeRefusal::InvalidBody),
        (
            Arc::from(MATCHING_HEALTH.replace("\"pid\":41", "\"pid\":42")),
            200,
            HealthProbeRefusal::IdentityMismatch,
        ),
    ] {
        let (endpoint, server) = serve_response(body, status, Duration::ZERO, Duration::ZERO).await;
        let outcome = matching_probe(endpoint).probe().await;
        join_server(server).await;
        assert_eq!(outcome, Err(expected));
    }
}

#[test]
fn empty_and_overlong_credentials_still_refuse() {
    let endpoint = required(Endpoint::new("http://127.0.0.1:1"), "endpoint");
    for (username, secret) in [
        (String::new(), "synthetic-secret".to_owned()),
        ("synthetic-user".to_owned(), String::new()),
        ("u".repeat(129), "synthetic-secret".to_owned()),
        ("synthetic-user".to_owned(), "s".repeat(257)),
    ] {
        let expected = match HealthExpectation::new(owner(endpoint.clone()), "build-a") {
            Ok(expected) => expected,
            Err(error) => panic!("expectation failed: {error}"),
        };
        assert_eq!(
            AuthenticatedHealthProbe::new(username, secret, expected).err(),
            Some(HealthProbeRefusal::InvalidCredential)
        );
    }
}

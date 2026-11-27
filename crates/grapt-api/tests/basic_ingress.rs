//! Witnesses for the one injected HTTP Basic ingress seam.

mod host_support;

#[path = "basic_ingress/stream_backpressure.rs"]
mod stream_backpressure;

use std::sync::Arc;
use std::time::Duration;

use grapt_api::auth::AuthenticationPolicyVersion;
use grapt_api::directory::{MaxPrincipalMappings, SeatDirectoryEntry, SeatPrincipalDirectory};
use grapt_api::router::{ApiRouterBuilder, MaxRouterModules, RouteMethod, RoutePath};
use grapt_api::{
    ApiHostLimits, AuthenticatedApiService, PreparedApiHost, RunningApiHost, SeatTrustBoundary,
    TokioDeadlineDriver,
};
use grapt_auth_basic::{BasicAuthError, BasicAuthVerifier, MAX_HEADER_BYTES};
use grapt_types::{ClockAuthority, DeterministicAuthorities, IdAuthority, IdToken, Timestamp};
use host_support::{limits_with, loopback_listener, RawConnection};

const VALID_AUTHORIZATION: &[u8] = b"Basic YWxwaGE6emVwaHly";

const _: () = assert!(grapt_api::MAX_SUBJECT_ID_BYTES >= grapt_auth_basic::MAX_USERNAME_BYTES);

#[derive(Debug)]
struct FixedClock;

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_700_000_000_000)
    }
}

#[derive(Debug)]
struct FixedIds;

impl IdAuthority for FixedIds {
    fn mint(&self) -> IdToken {
        match IdToken::new("basic-ingress-fixed-id") {
            Some(token) => token,
            None => panic!("the fixture id authority produced an invalid token"),
        }
    }
}

fn admitted<T>(value: Result<T, impl std::fmt::Debug>, what: &str) -> T {
    match value {
        Ok(value) => value,
        Err(error) => panic!("the fixture {what} was refused: {error:?}"),
    }
}

fn boundary() -> SeatTrustBoundary {
    let clock = FixedClock;
    let ids = FixedIds;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let max_mappings = admitted(MaxPrincipalMappings::try_new(4), "mapping bound");
    let subject = admitted(grapt_api::auth::SubjectId::try_new("alpha"), "subject");
    let recipient = admitted(
        grapt_types::SeatRecipientId::try_new("seat-alpha"),
        "recipient",
    );
    let entries = vec![SeatDirectoryEntry::new(subject, recipient)];
    let directory = admitted(
        SeatPrincipalDirectory::configure(entries, max_mappings, &authorities),
        "directory",
    );
    let policy = admitted(
        AuthenticationPolicyVersion::try_new("basic-auth-policy-v1"),
        "policy",
    );
    let verifier = admitted(BasicAuthVerifier::new("alpha", "zephyr"), "verifier");
    SeatTrustBoundary::with_injected_basic_verifier(policy, directory, verifier)
}

async fn unreachable_route() -> String {
    panic!("an unauthenticated request invoked a route before authentication succeeded")
}

async fn authenticated_route() -> String {
    "authenticated".to_owned()
}

fn refusing_service(limits: &ApiHostLimits) -> AuthenticatedApiService {
    let max_modules = admitted(MaxRouterModules::try_new(4), "module bound");
    let mut builder = ApiRouterBuilder::new(max_modules);
    let path = admitted(RoutePath::try_new("/witness"), "route path");
    admitted(
        builder.register(RouteMethod::Get, path, unreachable_route),
        "route registration",
    );
    AuthenticatedApiService::new(
        builder.seal(),
        Arc::new(boundary()),
        limits.max_request_body_bytes,
    )
}

fn accepting_service(limits: &ApiHostLimits) -> AuthenticatedApiService {
    let max_modules = admitted(MaxRouterModules::try_new(4), "module bound");
    let mut builder = ApiRouterBuilder::new(max_modules);
    let path = admitted(RoutePath::try_new("/witness"), "route path");
    admitted(
        builder.register(RouteMethod::Get, path, authenticated_route),
        "route registration",
    );
    AuthenticatedApiService::new(
        builder.seal(),
        Arc::new(boundary()),
        limits.max_request_body_bytes,
    )
}

async fn start(service: AuthenticatedApiService, limits: ApiHostLimits) -> RunningApiHost {
    let listener = loopback_listener().await;
    PreparedApiHost::new(
        listener,
        service,
        limits,
        Arc::new(TokioDeadlineDriver::new()),
    )
    .unwrap_or_else(|error| panic!("fixture host limits were refused: {error}"))
    .start()
}

fn request(authorization: &[u8]) -> Vec<u8> {
    let mut request = b"GET /witness HTTP/1.1\r\nHost: witness\r\nAuthorization: ".to_vec();
    request.extend_from_slice(authorization);
    request.extend_from_slice(b"\r\n\r\n");
    request
}

#[tokio::test]
async fn a_valid_basic_request_reaches_the_only_authenticated_route() {
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(accepting_service(&limits), limits).await;
    let mut connection = RawConnection::open(host.ready().observed_address()).await;
    connection.send(&request(VALID_AUTHORIZATION)).await;
    assert_eq!(connection.read_status().await, Some(200));
    drop(connection);
    let _ = host.stop().await;
}

#[tokio::test]
async fn the_retained_desktop_preflight_is_finite_and_invokes_no_route() {
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(refusing_service(&limits), limits).await;
    let address = host.ready().observed_address();

    let mut admitted = RawConnection::open(address).await;
    admitted
        .send(
            b"OPTIONS /witness HTTP/1.1\r\nHost: witness\r\nOrigin: http://tauri.localhost\r\n\
              Access-Control-Request-Method: GET\r\n\
              Access-Control-Request-Headers: authorization, content-type, x-grapt-directory, x-grapt-workspace, x-secode-directory, x-secode-workspace\r\n\r\n", // grapt-rename: keep
        )
        .await;
    let head = admitted
        .read_head()
        .await
        .unwrap_or_else(|| panic!("the desktop preflight returned no response"));
    assert_eq!(
        head.lines().next(),
        Some("HTTP/1.1 204 No Content"),
        "the desktop preflight was not admitted before authentication"
    );
    assert!(head.contains("access-control-allow-origin: http://tauri.localhost\r\n"));
    let allowed: Vec<_> = head
        .lines()
        .filter_map(|line| line.strip_prefix("access-control-allow-headers: "))
        .collect();
    assert_eq!(allowed.len(), 1);
    let names: std::collections::BTreeSet<_> = allowed[0].split(',').map(str::trim).collect();
    assert_eq!(
        names,
        std::collections::BTreeSet::from([
            "authorization",
            "content-type",
            "x-grapt-directory",
            "x-grapt-workspace",
            "x-secode-directory", // grapt-rename: keep
            "x-secode-workspace", // grapt-rename: keep
        ])
    );
    drop(admitted);

    let mut foreign = RawConnection::open(address).await;
    foreign
        .send(
            b"OPTIONS /witness HTTP/1.1\r\nHost: witness\r\nOrigin: https://attacker.invalid\r\n\
              Access-Control-Request-Method: GET\r\n\r\n",
        )
        .await;
    let head = foreign
        .read_head()
        .await
        .unwrap_or_else(|| panic!("the foreign preflight returned no refusal"));
    assert_eq!(head.lines().next(), Some("HTTP/1.1 401 Unauthorized"));
    assert!(!head.contains("access-control-allow-origin"));
    drop(foreign);
    let _ = host.stop().await;
}

#[tokio::test]
async fn a_repeated_desktop_origin_is_never_admitted() {
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(refusing_service(&limits), limits).await;
    let mut connection = RawConnection::open(host.ready().observed_address()).await;
    connection
        .send(
            b"OPTIONS /witness HTTP/1.1\r\nHost: witness\r\nOrigin: http://tauri.localhost\r\n\
              Origin: https://attacker.invalid\r\n\
              Access-Control-Request-Method: GET\r\n\r\n",
        )
        .await;
    let head = connection
        .read_head()
        .await
        .unwrap_or_else(|| panic!("the repeated-origin request returned no response"));
    assert_eq!(
        head.lines().next(),
        Some("HTTP/1.1 401 Unauthorized"),
        "a repeated desktop origin was admitted"
    );
    assert!(
        !head.contains("access-control-allow-origin"),
        "a repeated desktop origin received an allow-origin response"
    );
    drop(connection);
    let _ = host.stop().await;
}

#[tokio::test]
async fn repeated_admitted_desktop_preflight_values_are_all_validated() {
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(refusing_service(&limits), limits).await;
    let mut connection = RawConnection::open(host.ready().observed_address()).await;
    connection
        .send(
            b"OPTIONS /witness HTTP/1.1\r\nHost: witness\r\nOrigin: http://tauri.localhost\r\n\
              Access-Control-Request-Method: GET\r\n\
              Access-Control-Request-Method: HEAD\r\n\
              Access-Control-Request-Headers: authorization\r\n\
              Access-Control-Request-Headers: content-type, X-Grapt-Directory, x-grapt-workspace\r\n\r\n",
        )
        .await;
    let head = connection
        .read_head()
        .await
        .unwrap_or_else(|| panic!("the repeated admitted preflight returned no response"));
    assert_eq!(
        head.lines().next(),
        Some("HTTP/1.1 204 No Content"),
        "every repeated method and header value was admitted but the preflight was refused"
    );
    drop(connection);
    let _ = host.stop().await;
}

#[tokio::test]
async fn one_forbidden_repeated_desktop_preflight_value_refuses_the_preflight() {
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(refusing_service(&limits), limits).await;
    let address = host.ready().observed_address();
    for request in [
        &b"OPTIONS /witness HTTP/1.1\r\nHost: witness\r\nOrigin: http://tauri.localhost\r\n\
          Access-Control-Request-Method: GET\r\n\
          Access-Control-Request-Method: CONNECT\r\n\r\n"[..],
        &b"OPTIONS /witness HTTP/1.1\r\nHost: witness\r\nOrigin: http://tauri.localhost\r\n\
          Access-Control-Request-Method: GET\r\n\
          Access-Control-Request-Headers: authorization\r\n\
          Access-Control-Request-Headers: x-forbidden\r\n\r\n"[..],
    ] {
        let mut connection = RawConnection::open(address).await;
        connection.send(request).await;
        let head = connection
            .read_head()
            .await
            .unwrap_or_else(|| panic!("the repeated forbidden preflight returned no response"));
        assert_eq!(
            head.lines().next(),
            Some("HTTP/1.1 403 Forbidden"),
            "a later forbidden repeated preflight value was ignored"
        );
        drop(connection);
    }
    let _ = host.stop().await;
}

#[tokio::test]
async fn packaged_renderer_requests_still_require_the_basic_credential() {
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(accepting_service(&limits), limits).await;
    let address = host.ready().observed_address();
    for origin in ["http://tauri.localhost", "oc://renderer"] {
        for (authorization, status) in [
            (
                "Authorization: Basic YWxwaGE6emVwaHly\r\n",
                "HTTP/1.1 200 OK",
            ),
            (
                "Authorization: Basic YmV0YTp6ZXBoeXI=\r\n",
                "HTTP/1.1 401 Unauthorized",
            ),
            ("", "HTTP/1.1 401 Unauthorized"),
        ] {
            let mut connection = RawConnection::open(address).await;
            connection.send(format!("GET /witness HTTP/1.1\r\nHost: witness\r\nOrigin: {origin}\r\n{authorization}\r\n").as_bytes()).await;
            let head = connection
                .read_head()
                .await
                .unwrap_or_else(|| panic!("no renderer response"));
            assert_eq!(head.lines().next(), Some(status));
            assert!(
                head.contains(&format!("access-control-allow-origin: {origin}\r\n")),
                "{head}"
            );
            assert!(head.contains("vary: Origin\r\n"), "{head}");
        }
    }
    let _ = host.stop().await;
}

#[test]
fn a_verified_subject_resolves_only_through_the_boundary_directory() {
    let boundary = boundary();
    let authorization = admitted(
        String::from_utf8(VALID_AUTHORIZATION.to_vec()),
        "authorization",
    );
    let credential = grapt_api::PresentedCredential::new(authorization);
    let principal = admitted(boundary.authenticate(&credential), "authentication");
    assert_eq!(principal.receipt().subject().as_str(), "alpha");
    assert_eq!(
        principal.receipt().policy().as_str(),
        "basic-auth-policy-v1"
    );
    assert_eq!(principal.recipient().as_str(), "seat-alpha");
    assert_eq!(principal.generation().value(), 1);
}

#[tokio::test]
async fn hostile_or_duplicate_authorization_never_reaches_a_route() {
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(refusing_service(&limits), limits).await;
    let address = host.ready().observed_address();
    let oversized = {
        let mut value = b"Basic ".to_vec();
        value.resize(MAX_HEADER_BYTES + 1, b'A');
        value
    };
    for authorization in [
        &b"Basic !!!! "[..],
        &b"Bearer YWxwaGE6emVwaHly"[..],
        &b"Basic YmV0YTp6ZXBoeXI="[..],
        oversized.as_slice(),
    ] {
        let mut connection = RawConnection::open(address).await;
        connection.send(&request(authorization)).await;
        assert_eq!(
            connection.read_status().await,
            Some(401),
            "hostile authorization must be refused by the trust boundary"
        );
        drop(connection);
    }

    let duplicate = b"GET /witness HTTP/1.1\r\nHost: witness\r\nAuthorization: Basic YWxwaGE6emVwaHly\r\nAuthorization: Basic YWxwaGE6emVwaHly\r\n\r\n";
    let mut connection = RawConnection::open(address).await;
    connection.send(duplicate).await;
    assert_eq!(connection.read_status().await, Some(401));
    drop(connection);
    let _ = host.stop().await;
}

async fn assert_credential_headers_are_refused_before_route_effects(request: &[u8]) {
    let limits = limits_with(64, 8192, Duration::from_secs(5));
    let host = start(refusing_service(&limits), limits).await;
    let address = host.ready().observed_address();
    let mut connection = RawConnection::open(address).await;
    connection.send(request).await;
    assert_eq!(
        connection.read_status().await,
        Some(401),
        "a legacy credential header must be refused before route invocation"
    );
    drop(connection);
    let _ = host.stop().await;
}

#[tokio::test]
async fn a_legacy_only_credential_is_refused_before_route_effects() {
    assert_credential_headers_are_refused_before_route_effects(
        b"GET /witness HTTP/1.1\r\nHost: witness\r\nx-grapt-seat-credential: Basic YWxwaGE6emVwaHly\r\n\r\n",
    )
    .await;
}

#[tokio::test]
async fn duplicate_legacy_credentials_are_refused_before_route_effects() {
    assert_credential_headers_are_refused_before_route_effects(
        b"GET /witness HTTP/1.1\r\nHost: witness\r\nx-grapt-seat-credential: Basic YWxwaGE6emVwaHly\r\nx-grapt-seat-credential: Basic YWxwaGE6emVwaHly\r\n\r\n",
    )
    .await;
}

#[tokio::test]
async fn combined_authorization_and_legacy_credentials_are_refused_before_route_effects() {
    assert_credential_headers_are_refused_before_route_effects(
        b"GET /witness HTTP/1.1\r\nHost: witness\r\nAuthorization: Basic YWxwaGE6emVwaHly\r\nx-grapt-seat-credential: Basic YWxwaGE6emVwaHly\r\n\r\n",
    )
    .await;
}

#[test]
fn basic_error_diagnostics_do_not_render_identity_material() {
    let error = BasicAuthError::Rejected;
    assert_eq!(error.to_string(), "the Basic credential did not verify");
}

//! The sealed authenticated service, and the explicit HTTP/1 builder.
//!
//! Law source: C2 (master spec L2942, "the answering transport supplies an authenticated
//! client identity; callers cannot self-assert it"), C3 (L2943, the closed principal
//! vocabulary), and the HTTP host contract — "direct
//! `hyper::server::conn::http1::Builder`, never `axum::serve`/auto/upgrades/HTTP2".
//!
//! # What the host is allowed to be handed
//!
//! [`ApiHost`](crate::host::PreparedApiHost) takes an
//! [`AuthenticatedApiService`] and refuses to take anything else. It cannot be
//! handed an `axum::Router`, a `tower` service, or a closure, and that is the
//! only thing standing between this lane and a caller who installs a layer that
//! invents a principal. The seal is three properties together:
//!
//! * the router field is private, and there is no `into_inner`, no
//!   `From<Router>`, no `Deref` and no unchecked constructor;
//! * the ONLY constructor takes a sealed [`ApiRouter`], so the route set it
//!   serves is one that A already closed;
//! * the principal reaches a route as [`SealedPrincipal`], whose constructor is
//!   crate-private, so no handler and no middleware can put a different one into
//!   a request.
//!
//! # This lane authenticates and refuses; it cannot allow
//!
//! Ruling R4HA-04 is why. `CredentialAdapter::verify` ships no credential
//! backend and refuses everything, so a production build of this service answers
//! `401` to every request and never invokes a route. That is not a gap in H — it
//! is A's accepted production behaviour, and H would be weakening the seal if it
//! added a way around it. The `#[cfg(test)]` ingress below reaches A's own
//! `authenticate_fixture`, which is the route A built for exactly this and which
//! compiles into no other configuration.
//!
//! # Why `axum::serve` is refused as the proof boundary
//!
//! It accepts and spawns with no host-owned permit, uses hyper-util's automatic
//! protocol builder rather than a caller-configured HTTP/1 one, hides the
//! connection set so no forced-close inventory can be counted, and folds no
//! connection error into a typed exit. Every bound below would become a claim
//! rather than a measurement.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use std::convert::Infallible;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::http::{HeaderValue, Method, Request, Response, StatusCode};
use axum::Router;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::Service;
use hyper_util::service::TowerToHyperService;
use tower_http::limit::RequestBodyLimit;

use crate::auth::{AuthenticatedSeatPrincipal, PresentedCredential, SeatTrustBoundary};
use crate::deadline::{header_read_timeout, HostTimer};
use crate::limits::{ApiHostLimits, MaxRequestBodyBytes};
use crate::router::ApiRouter;

/// The legacy header retained only for this crate's trusted fixture ingress.
///
/// Production refuses every request that contains this header, including a
/// request that also contains the one admitted `Authorization` header.
// The pre-rename location header spellings an older renderer may still send.
const LEGACY_DIRECTORY_HEADER: &str = "x-secode-directory"; // grapt-rename: keep
const LEGACY_WORKSPACE_HEADER: &str = "x-secode-workspace"; // grapt-rename: keep
pub const SEAT_CREDENTIAL_HEADER: &str = "x-grapt-seat-credential";
/// The only credential header admitted by production ingress.
pub const AUTHORIZATION_HEADER: &str = "authorization";
/// The retained Tauri renderer's WebView2 origin.
///
/// Exact rather than reflected: the desktop bridge carries a Basic credential,
/// so admitting an arbitrary browser origin would hand a local authenticated
/// API to any page that could make the request.
pub const DESKTOP_RENDERER_ORIGIN: &str = "http://tauri.localhost";
/// The packaged Electron renderer origin.
pub const ELECTRON_RENDERER_ORIGIN: &str = "oc://renderer";

/// The server-derived principal, as a route sees it.
///
/// A route reads this out of the request extensions and can read it nowhere
/// else. The constructor is crate-private, so this type is unforgeable outside
/// the crate for the same reason the principal inside it is — and being a
/// distinct type means the extension key itself is H-owned, so no dependent can
/// insert a look-alike.
///
/// `Clone` is on the handle, never on the principal. `http::Extensions` requires
/// `Clone`, and [`AuthenticatedSeatPrincipal`] deliberately is not: one
/// verification mints one principal. An `Arc` hands out additional REFERENCES to
/// that one principal, which is a different thing from minting a second, and the
/// generation check still governs every use.
#[derive(Debug, Clone)]
pub struct SealedPrincipal(Arc<AuthenticatedSeatPrincipal>);

impl SealedPrincipal {
    pub(crate) fn seal(principal: AuthenticatedSeatPrincipal) -> Self {
        Self(Arc::new(principal))
    }

    /// The principal this boundary derived.
    pub fn principal(&self) -> &AuthenticatedSeatPrincipal {
        &self.0
    }
}

/// How a credential becomes a verified subject.
///
/// Two arms, and the second exists only in this crate's own test build. It is
/// not a feature — features are additive, so any dependent could turn one on —
/// and not `doc(hidden)`, which is reachable too.
enum Ingress {
    /// A's production path: verify the presented credential.
    Production,
    /// A's `#[cfg(test)]` fixture path, reached only from this crate's own
    /// library test binary.
    #[cfg(test)]
    TrustedFixture,
}

impl fmt::Debug for Ingress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ingress::Production => f.write_str("Production"),
            #[cfg(test)]
            Ingress::TrustedFixture => f.write_str("TrustedFixture"),
        }
    }
}

/// The composed, sealed service the host serves.
struct Inner {
    routed: TowerToHyperService<RequestBodyLimit<Router<()>>>,
    image_prompts: Option<TowerToHyperService<RequestBodyLimit<Router<()>>>>,
    boundary: Arc<SeatTrustBoundary>,
    ingress: Ingress,
}

/// An A-sealed route set behind this boundary's own credential ingress.
///
/// Cloning is cheap and shares one composition; the route set and the trust
/// boundary are behind one `Arc` and neither is reachable through this type.
#[derive(Clone)]
pub struct AuthenticatedApiService {
    inner: Arc<Inner>,
}

impl fmt::Debug for AuthenticatedApiService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Neither the router nor the boundary is rendered. `Router` is not
        // `Debug` at all, and a boundary's directory names real subjects.
        f.debug_struct("AuthenticatedApiService")
            .field("ingress", &self.inner.ingress)
            .finish()
    }
}

impl AuthenticatedApiService {
    /// The only production constructor.
    ///
    /// Takes the SEALED [`ApiRouter`], not an `axum::Router`. A caller that
    /// holds a raw router cannot reach this function, which is what probe
    /// `07_raw_router_cannot_start_host` witnesses.
    ///
    /// The body bound is applied here rather than in the host, because it is a
    /// property of the request as the SERVICE sees it: a known oversized
    /// `Content-Length` is refused before the inner route, and an unknown length
    /// is wrapped so that no route can observe bytes past the bound.
    pub fn new(
        router: ApiRouter,
        boundary: Arc<SeatTrustBoundary>,
        max_request_body_bytes: MaxRequestBodyBytes,
    ) -> Self {
        Self::compose(
            router,
            boundary,
            max_request_body_bytes,
            Ingress::Production,
            false,
        )
    }

    /// Admit bounded inline images only on the ordinary authenticated prompt route.
    /// Other routes retain the caller's ordinary body bound.
    pub fn with_inline_image_prompts(
        router: ApiRouter,
        boundary: Arc<SeatTrustBoundary>,
        max_request_body_bytes: MaxRequestBodyBytes,
    ) -> Self {
        Self::compose(
            router,
            boundary,
            max_request_body_bytes,
            Ingress::Production,
            true,
        )
    }

    fn compose(
        router: ApiRouter,
        boundary: Arc<SeatTrustBoundary>,
        max_request_body_bytes: MaxRequestBodyBytes,
        ingress: Ingress,
        inline_images: bool,
    ) -> Self {
        let router = router.into_router();
        let image_prompts = inline_images.then(|| {
            TowerToHyperService::new(RequestBodyLimit::new(router.clone(), 4 * 1024 * 1024))
        });
        let limited = RequestBodyLimit::new(router, max_request_body_bytes.value());
        Self {
            inner: Arc::new(Inner {
                routed: TowerToHyperService::new(limited),
                image_prompts,
                boundary,
                ingress,
            }),
        }
    }

    /// The same composition against A's own fixture verification route.
    ///
    /// `#[cfg(test)]` for the reason A gives for `verify_fixture`: this compiles
    /// into this crate's library test binary and into nothing else, so it is not
    /// a second public route to authority wearing a different name.
    #[cfg(test)]
    pub(crate) fn fixture(
        router: ApiRouter,
        boundary: Arc<SeatTrustBoundary>,
        max_request_body_bytes: MaxRequestBodyBytes,
    ) -> Self {
        Self::compose(
            router,
            boundary,
            max_request_body_bytes,
            Ingress::TrustedFixture,
            false,
        )
    }

    /// Derive the principal for one request, or refuse.
    ///
    /// Production reads exactly one `Authorization` header and refuses every
    /// presence of the legacy fixture header. Note what this signature does not
    /// do: it never consults the request body, path or query, so there is nowhere
    /// for a caller-supplied recipient or client id to enter.
    fn authenticate(&self, request: &Request<Incoming>) -> Option<SealedPrincipal> {
        match self.inner.ingress {
            Ingress::Production => {
                if request.headers().contains_key(SEAT_CREDENTIAL_HEADER) {
                    return None;
                }
                let mut authorization = request.headers().get_all(AUTHORIZATION_HEADER).iter();
                let value = authorization.next()?;
                if authorization.next().is_some() {
                    return None;
                }
                let presented = PresentedCredential::new_raw(value.as_bytes());
                self.inner
                    .boundary
                    .authenticate(&presented)
                    .ok()
                    .map(SealedPrincipal::seal)
            }
            #[cfg(test)]
            Ingress::TrustedFixture => {
                let subject = request
                    .headers()
                    .get(SEAT_CREDENTIAL_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| crate::auth::SubjectId::try_new(value).ok())?;
                self.inner
                    .boundary
                    .authenticate_fixture(subject)
                    .ok()
                    .map(SealedPrincipal::seal)
            }
        }
    }
}

/// The refusal an unauthenticated request receives.
///
/// One constant typed `401` body. It names no subject, no bound and no reason: A's
/// whole error thesis is that a refusal must not become an enumeration oracle,
/// and a transport that helpfully distinguished "unknown subject" from "bad
/// credential" would rebuild the oracle A removed.
fn unauthenticated() -> Response<Body> {
    crate::u3a::unauthorized_response()
}

#[derive(Clone, Copy)]
enum DesktopOrigin {
    Tauri,
    Electron,
}

impl DesktopOrigin {
    fn classify(request: &Request<Incoming>) -> Option<Self> {
        let mut origins = request.headers().get_all("origin").iter();
        let origin = match origins.next()?.as_bytes() {
            value if value == DESKTOP_RENDERER_ORIGIN.as_bytes() => Self::Tauri,
            value if value == ELECTRON_RENDERER_ORIGIN.as_bytes() => Self::Electron,
            _ => return None,
        };
        origins.next().is_none().then_some(origin)
    }

    fn add_to(self, response: &mut Response<Body>) {
        let origin = match self {
            Self::Tauri => DESKTOP_RENDERER_ORIGIN,
            Self::Electron => ELECTRON_RENDERER_ORIGIN,
        };
        response.headers_mut().insert(
            "access-control-allow-origin",
            HeaderValue::from_static(origin),
        );
        response
            .headers_mut()
            .insert("vary", HeaderValue::from_static("Origin"));
    }
}

/// Answer the browser's credentialed preflight before authentication.
///
/// This is not an alternate route to authority: OPTIONS invokes no route and
/// receives no principal. It only admits the two packaged renderer origins and
/// the finite method/header vocabulary the public API already exposes.
fn desktop_preflight(request: &Request<Incoming>) -> Option<Response<Body>> {
    if request.method() != Method::OPTIONS {
        return None;
    }
    let origin = DesktopOrigin::classify(request)?;
    let method_admitted = {
        let mut methods = request
            .headers()
            .get_all("access-control-request-method")
            .iter();
        let admitted = |value: &HeaderValue| {
            value.to_str().ok().is_some_and(|value| {
                matches!(value, "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE")
            })
        };
        methods.next().is_some_and(admitted) && methods.all(admitted)
    };
    let headers_admitted = request
        .headers()
        .get_all("access-control-request-headers")
        .iter()
        .all(|value| {
            value.to_str().is_ok_and(|value| {
                value.split(',').all(|header| {
                    matches!(
                        header.trim().to_ascii_lowercase().as_str(),
                        "authorization"
                            | "content-type"
                            | "x-grapt-directory"
                            | "x-grapt-workspace"
                            | LEGACY_DIRECTORY_HEADER
                            | LEGACY_WORKSPACE_HEADER
                    )
                })
            })
        });
    let mut response = Response::new(Body::empty());
    *response.status_mut() = if method_admitted && headers_admitted {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::FORBIDDEN
    };
    if !method_admitted || !headers_admitted {
        return Some(response);
    }
    origin.add_to(&mut response);
    response.headers_mut().insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, HEAD, POST, PUT, PATCH, DELETE"),
    );
    response.headers_mut().insert(
        "access-control-allow-headers",
        HeaderValue::from_static(
            "authorization, content-type, x-grapt-directory, x-grapt-workspace, \
             x-secode-directory, x-secode-workspace", // grapt-rename: keep
        ),
    );
    Some(response)
}

impl Service<Request<Incoming>> for AuthenticatedApiService {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, mut request: Request<Incoming>) -> Self::Future {
        if let Some(response) = desktop_preflight(&request) {
            return Box::pin(core::future::ready(Ok(response)));
        }
        let desktop_origin = DesktopOrigin::classify(&request);
        let Some(principal) = self.authenticate(&request) else {
            // The route is not invoked at all. Refuse-before-effect: there is no
            // partially authenticated state for a handler to be lenient about.
            let mut response = unauthenticated();
            if let Some(origin) = desktop_origin {
                origin.add_to(&mut response);
            }
            return Box::pin(core::future::ready(Ok(response)));
        };
        // The server-derived principal REPLACES anything a caller might have
        // sent, because it is inserted after the request is read and nothing
        // else in this crate inserts this type. `insert` returning a previous
        // value is impossible for a type no caller can construct.
        request.extensions_mut().insert(principal);
        let image_route = request.method() == Method::POST
            && request
                .uri()
                .path()
                .strip_prefix("/api/session/")
                .is_some_and(|rest| {
                    rest.strip_suffix("/prompt")
                        .is_some_and(|session| !session.is_empty() && !session.contains('/'))
                });
        let routed = if image_route {
            self.inner
                .image_prompts
                .as_ref()
                .unwrap_or(&self.inner.routed)
        } else {
            &self.inner.routed
        };
        let future = routed.call(request);
        Box::pin(async move {
            let mut response = future.await?.map(Body::new);
            if let Some(origin) = desktop_origin {
                origin.add_to(&mut response);
            }
            Ok(response)
        })
    }
}

/// The explicitly configured HTTP/1 connection builder.
///
/// Every value comes from an already-checked limit, which is what keeps
/// `max_buf_size`'s assertion and `serve_connection`'s missing-timer panic
/// unreachable. Nothing here is left at a hyper default that H claims to have
/// set.
pub(crate) fn connection_builder(limits: &ApiHostLimits, timer: HostTimer) -> http1::Builder {
    let mut builder = http1::Builder::new();
    builder
        // The timer FIRST, because `header_read_timeout` without one makes
        // `serve_connection` panic, and ordering the two this way means the
        // panic condition never exists rather than being avoided.
        .timer(timer)
        .max_headers(limits.max_headers.value())
        .max_buf_size(limits.max_request_head_bytes.value())
        .header_read_timeout(header_read_timeout(limits.header_read_allowance))
        // Explicit rather than inherited. Keep-alive stays ON and the
        // per-connection request budget is what bounds reuse; turning it off
        // here would make that budget unobservable, because every connection
        // would serve exactly one request whatever the number said.
        .keep_alive(true);
    builder
}

/// Asserts at compile time that the served body is what hyper requires.
///
/// A `const` binding rather than an uncalled function, so it needs no
/// `dead_code` allowance: this lane's architecture gate refuses lint
/// suppression, and an assertion that had to be excused would be the first
/// exception.
const _: fn() = || {
    fn assert_hyper_body<B: hyper::body::Body<Data = Bytes>>() {}
    assert_hyper_body::<crate::admission::PermitBody>();
};

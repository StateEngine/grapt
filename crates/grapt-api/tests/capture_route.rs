//! J03 route-composition witnesses - campaign EVENT-STREAM-REPLAY-CURSOR-01.
//!
//! The authenticated capture stream is additive: it composes into the same
//! router as the frozen v2 `/api/event` route, and a second registration of
//! the same (method, path) pair is refused by the builder before anything is
//! sealed. These witnesses need only the public surface, so they live here
//! rather than in the library's `#[cfg(test)]` module.

use std::sync::Arc;

use grapt_api::u3a::{DtoString, ServerConnectedEvent};
use grapt_api::{
    register_capture_event_route, register_dogfood_event_route, ApiRefusal, ApiRouterBuilder,
    ApplicationFuture, CaptureApplicationPort, CapturePage, CapturePageError, DogfoodEventHub,
    MaxRouterModules, SealedPrincipal, SseLimits, WireId, CAPTURE_EVENT_PATH, DOGFOOD_EVENT_PATH,
};
use grapt_types::{CaptureCursor, CaptureInterest};

/// A no-op application port: the route-composition witnesses never serve a
/// durable read, so every read refuses as an unreadable store rather than
/// inventing one.
struct UnreadableApplication;

impl CaptureApplicationPort for UnreadableApplication {
    fn capture_page(
        &self,
        _principal: SealedPrincipal,
        _interest: CaptureInterest,
        _cursor: CaptureCursor,
    ) -> ApplicationFuture<CapturePage, CapturePageError> {
        Box::pin(async {
            Err(CapturePageError::UnreadableStore {
                detail: dto_string("the composition witness serves no store"),
            })
        })
    }

    fn count_capture_aggregates(
        &self,
        _principal: SealedPrincipal,
        _interest: CaptureInterest,
    ) -> ApplicationFuture<u64, CapturePageError> {
        Box::pin(async {
            Err(CapturePageError::UnreadableStore {
                detail: dto_string("the composition witness serves no store"),
            })
        })
    }
}

fn dto_string(raw: &str) -> DtoString {
    match DtoString::try_new(raw) {
        Ok(value) => value,
        Err(error) => panic!("fixture string {raw:?} was refused: {error}"),
    }
}

fn bound(value: usize) -> MaxRouterModules {
    match MaxRouterModules::try_new(value) {
        Ok(bound) => bound,
        Err(refusal) => panic!("fixture bound {value} was refused: {refusal}"),
    }
}

fn hub_fixture() -> Arc<DogfoodEventHub> {
    let limits = match SseLimits::production() {
        Ok(limits) => limits,
        Err(error) => panic!("SSE limits failed: {error}"),
    };
    let Some(id) = WireId::parse("evt_hub".to_owned()) else {
        panic!("the hub identity fixture was refused")
    };
    let hub = match DogfoodEventHub::try_new(limits, ServerConnectedEvent::new(id)) {
        Ok(hub) => hub,
        Err(error) => panic!("event hub failed: {error}"),
    };
    Arc::new(hub)
}

#[test]
fn capture_and_v2_routes_compose_into_one_router() {
    let mut builder = ApiRouterBuilder::new(bound(16));
    let hub = hub_fixture();
    if let Err(refusal) = register_dogfood_event_route(&mut builder, Arc::clone(&hub)) {
        panic!("the v2 event route was refused: {refusal}");
    }
    let port = Arc::new(UnreadableApplication);
    if let Err(refusal) = register_capture_event_route(&mut builder, Arc::clone(&hub), port) {
        panic!("the capture route was refused: {refusal}");
    }
    let routes = builder.seal().route_set();
    assert!(
        routes.contains(&format!("GET {DOGFOOD_EVENT_PATH}")),
        "the frozen v2 route is missing from the composed set: {routes:?}"
    );
    assert!(
        routes.contains(&format!("GET {CAPTURE_EVENT_PATH}")),
        "the capture route is missing from the composed set: {routes:?}"
    );
}

#[test]
fn a_duplicate_capture_registration_refuses() {
    let mut builder = ApiRouterBuilder::new(bound(16));
    let hub = hub_fixture();
    let port = Arc::new(UnreadableApplication);
    if let Err(refusal) = register_capture_event_route(&mut builder, Arc::clone(&hub), port) {
        panic!("the first capture registration was refused: {refusal}");
    }
    let port = Arc::new(UnreadableApplication);
    match register_capture_event_route(&mut builder, Arc::clone(&hub), port) {
        Ok(()) => panic!("a duplicate capture registration must be refused"),
        Err(refusal) => assert!(
            matches!(refusal, ApiRefusal::DuplicateRoute { .. }),
            "the duplicate used an unrelated refusal: {refusal:?}"
        ),
    }
}

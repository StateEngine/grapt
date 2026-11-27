// Probe 11 (R4H-H): a raw router cannot become an authenticated service.
//
// This is the seal the whole lane rests on. If the host could be handed a bare
// router — or a generic caller-supplied service — then a caller could install a
// layer that invents a principal, or route around the credential ingress
// entirely, and every bound below it would be measuring the wrong pipeline.
//
// The raw router here is obtained from A's OWN `ApiRouter::into_router`, which
// is the strongest form of the probe: even the router that A itself unsealed
// cannot be passed where the sealed one is required. The type is never named,
// which is deliberate — naming it would prove something about an import rather
// than about the constructor's signature.
use std::sync::Arc;

use grapt_api::{ApiRouter, AuthenticatedApiService, MaxRequestBodyBytes, SeatTrustBoundary};

fn sealed() -> ApiRouter {
    unimplemented!("the probe never runs; only its types are checked")
}

fn boundary() -> Arc<SeatTrustBoundary> {
    unimplemented!("the probe never runs; only its types are checked")
}

fn body_bound() -> MaxRequestBodyBytes {
    unimplemented!("the probe never runs; only its types are checked")
}

fn main() {
    let raw = sealed().into_router();
    let _service = AuthenticatedApiService::new(raw, boundary(), body_bound());
}

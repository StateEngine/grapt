// Probe 09 (R4H-H): a listener has one owner, and the host takes it.
//
// The accepted location crate makes `BoundListener` non-`Clone` so that exactly
// one owner is responsible for closing the socket. That guarantee only survives
// if the host takes it BY VALUE — a host that borrowed it would leave the caller
// holding a second way to accept on the same socket, and two accept owners on
// one listener is a race no receipt can describe.
//
// The observable consequence of this same law is witnessed separately in
// `tests/shutdown_contract.rs`: after the host joins, the address it was serving
// can be rebound. A compile error and a rebind are two different halves — one
// proves the caller cannot keep the handle, the other proves the host really
// gave it up.
use std::sync::Arc;

use grapt_api::{ApiHostLimits, AuthenticatedApiService, DeadlineDriver, PreparedApiHost};
use grapt_location::BoundListener;

fn listener() -> BoundListener {
    unimplemented!("the probe never runs; only its types are checked")
}

fn service() -> AuthenticatedApiService {
    unimplemented!("the probe never runs; only its types are checked")
}

fn limits() -> ApiHostLimits {
    unimplemented!("the probe never runs; only its types are checked")
}

fn driver() -> Arc<dyn DeadlineDriver> {
    unimplemented!("the probe never runs; only its types are checked")
}

fn main() {
    let listener = listener();
    let _host = PreparedApiHost::new(listener, service(), limits(), driver());
    // The listener moved into the host on the line above.
    let _receipt = listener.receipt();
}

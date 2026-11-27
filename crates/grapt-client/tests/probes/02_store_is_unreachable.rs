// Probe 02: durable truth is unreachable except over the wire.
//
// `grapt-store` owns the event log this client renders. Reading it directly
// would let the client show history the wire never carried, and would make
// every reconnect and restart witness in this crate certify a path the remote
// transport never takes.
extern crate grapt_store;

fn main() {
    let _bypass: Option<grapt_store::EventStore> = None;
}

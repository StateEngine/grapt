// Probe 01: the session supervisor is unreachable from the terminal client.
//
// `grapt-runtime` owns the session inbox and step scheduling. An embedded
// client shares its process, so calling it directly would be faster than
// encoding a request and decoding a response — which is precisely the
// superseded reading the I1 row retires.
//
// If this file compiles, someone added the dependency.
use grapt_runtime::session_ports::SessionPorts;

fn main() {
    let _bypass: Option<SessionPorts> = None;
}

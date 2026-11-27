// Probe 10 (R4H-H): a running host has exactly one owner.
//
// `stop` consumes the owner and returns the receipt. A `Clone` would produce a
// second owner that could stop the same host again — and the second stop would
// join a task that had already been taken, so one of the two callers would get
// a receipt describing a host it did not stop.
//
// Asserted through a `T: Clone` BOUND rather than by calling `.clone()`, for the
// reason probe 03 records: autoref makes `value.clone()` resolve on `&T` and
// compile even when `T` is not `Clone`, so a `.clone()` call is a witness that
// passes for the wrong reason.
use grapt_api::RunningApiHost;

fn requires_clone<T: Clone>() {}

fn main() {
    requires_clone::<RunningApiHost>();
}

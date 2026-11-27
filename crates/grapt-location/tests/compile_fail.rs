//! Compile-time probes for the two seams that a runtime assertion cannot hold.
//!
//! The compiler must REJECT each probe file; a probe that starts compiling is a
//! broken law.
//!
//! | Probe | Law |
//! | --- | --- |
//! | `01_fixed_port_rejects_zero` | `BindPort::Fixed` carries a `NonZeroU16`, so a zero fixed port is not merely refused at runtime — it cannot be written |
//! | `02_bound_listener_not_clone` | `BoundListener` is sole ownership of one socket; a second owner cannot be created |
//! | `03_accepted_connection_not_clone` | `AcceptedConnection` is sole ownership of one admitted stream; the accept seam cannot hand back a duplicable connection |
//!
//! # Why these are two files and not one
//!
//! rustc reports what it reaches, so a probe carrying two faults witnesses only
//! the first and silently covers for the second. These fail at different phases
//! on different items and neither law implies the other: a crate that took
//! `Option<u16>` for the port but kept sole listener ownership passes 02 and
//! fails 01, and one with a nonzero port type that derived `Clone` on the
//! listener passes 01 and fails 02. Each `.stderr` was read and contains
//! exactly one `error[` line.
//!
//! # What these witness, stated honestly
//!
//! Neither probe is delta-sensitive against the base commit, and claiming
//! otherwise would be a false receipt: at `0b069dbd` the crate does not exist,
//! so both fail there with an unresolved import, which witnesses nothing.
//!
//! What they witness is REQUIREDNESS. Both laws are carried by the type system,
//! which means there is no runnable mutant for the mutation harness to score:
//! widening `Fixed` back to a plain `u16`, or deriving `Clone` on the listener,
//! does not produce a program that compiles and fails a test — it produces a
//! program in which these probes compile. That is precisely the failure these
//! files detect, and it is why the QL battery does not carry a duplicate row
//! for either seam.

#[test]
fn location_probes_are_rejected_by_the_compiler() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}

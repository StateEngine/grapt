// Probe 01 (R4H ruling 02): a fixed bind port cannot be zero.
//
// The frozen oracle carries "no port", "port 0" and "search from 4096" in one
// nullable integer, so an explicit zero and a missing value are the same bytes.
// `BindPort::Fixed(NonZeroU16)` makes the collapse unwritable: zero is not a
// value this variant admits, so no runtime check can be forgotten or bypassed.
//
// This probe carries exactly one error phase. The port literal is otherwise
// perfectly ordinary; the only fault is that zero has no `NonZeroU16`.

use grapt_location::BindPort;

fn main() {
    // Asking for "the fixed port zero" — the sentinel this crate refuses to own.
    let _ = BindPort::Fixed(0);
}

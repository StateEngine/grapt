// Probe 60: the unkeyed digest constructor is unreachable from a consumer.
//
// `Digest::of_bytes` was public, and its own doc named its inputs: "a config
// blob, an env value, a credential". With `to_hex` beside it and the digest
// published in the receipt, `Digest::of_bytes(guess).to_hex()` was a complete
// offline guessing oracle against a public field.
//
// The type no longer leaves the crate at all, so the probe fails at the path
// rather than at the method. That is deliberate and stronger: demoting the two
// methods while re-exporting the type would leave the next author one `pub fn`
// away from reopening this, whereas an unexported type has no such surface.
fn main() {
    let _ = grapt_policy::Digest::of_bytes(b"a candidate secret");
}

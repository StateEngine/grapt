// Probe 05 (R4H-A): no DTO is, carries, or yields an identity.
//
// A DTO is caller-authored data. If a decoded envelope could hand back a
// principal, a verified subject or a lease, then the caller would be authoring
// authority and every seal upstream of it would be decoration.
//
// This is a probe rather than a runtime test because the property is an
// ABSENCE, and a runtime test can only ever observe the accessors somebody
// thought to write.
use grapt_api::RequestEnvelope;

fn main() {
    let envelope: RequestEnvelope = todo!();
    let _identity = envelope.principal();
}

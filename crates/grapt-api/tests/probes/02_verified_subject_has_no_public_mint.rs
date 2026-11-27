// Probe 02 (R4H-A): the INPUT to the mint is sealed too.
//
// Sealing the principal's mint alone would be a half-seal. `resolve` takes a
// `VerifiedSubject`, so a public route to a verified subject is a public route
// to a principal one call later — which is why the boundary is stated as three
// obligations rather than one, and why "no public `from_verified_subject(subject:
// String)`" is the specific shape called out as shipping nothing.
use grapt_api::VerifiedSubject;

fn main() {
    let _forged = VerifiedSubject::mint(todo!(), todo!());
}

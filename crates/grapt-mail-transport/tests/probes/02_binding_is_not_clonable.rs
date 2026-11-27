// Probe 2: a MailBinding cannot be duplicated.
//
// Ruling J2 removed leases and fences, so nothing rejects a stale handle at the
// provider. That makes non-duplication structural rather than incidental: the
// port's `close` takes the binding BY VALUE precisely so a closed binding cannot
// be used again, and a `Clone` would hand the caller a second handle that
// outlives the close.
//
// One error phase: method resolution (E0599) on the missing `clone`.
use grapt_mail_transport::binding::MailBinding;

fn main() {
    let binding: MailBinding = todo!();
    let _second = binding.clone();
}

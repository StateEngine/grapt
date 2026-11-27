// Probe 3: an external crate cannot construct a MailBinding.
//
// A MailBinding is this crate's proof that an AUTHENTICATED open happened, and
// R3b-2B/2C are expected to read `receipt().verified_principal()` as authority.
// The R1 review compiled exactly this file against the previous candidate: it
// forged a binding naming an arbitrary principal, with no adapter and no
// verification of any kind, and it PASSED. `verify_expectations` cannot catch
// that — it compares a request against a receipt the same caller authored, so on
// a forged binding the check is vacuous.
//
// `MailBinding::new` and `MailBindingReceipt::new` are now `pub(crate)`, as
// narrowly as the deterministic fake and a future in-crate adapter require.
//
// One error phase: privacy (E0603/E0624) on the crate-owned constructor. The
// separate law — that the PORT cannot be implemented externally — is probe 04,
// because rustc stops at the first phase and one file would witness only one.
use grapt_mail_transport::binding::MailBinding;

fn main() {
    let receipt = todo!();
    let _forged = MailBinding::new(receipt);
}

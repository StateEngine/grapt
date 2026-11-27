// Probe 1: a delivery/admission acknowledgement cannot express attention.
//
// `msg_ack` on the existing floor means READ. This contract exists because
// "it arrived durably" and "someone attended to it" are different facts, and a
// transport able to reach a read accessor from a delivery ack could settle
// attention nobody paid — silently, during automatic catch-up.
//
// One error phase: every call below fails method resolution (E0599).
use grapt_mail_transport::ack::{DeliveryAckOutcome, DeliveryAdmissionAck};

fn main() {
    let ack: DeliveryAdmissionAck = todo!();
    let outcome: DeliveryAckOutcome = todo!();

    // There is no read state to observe...
    let _ = ack.read();
    let _ = ack.is_read();
    let _ = ack.handled();
    // ...none to set...
    ack.mark_read();
    // ...and an outcome does not carry one either.
    let _ = outcome.read_by();
}

// A receipt cannot exist without its reservation.
//
// R2C5A0: "request sent for an attempt that was never reserved" must be
// UNREPRESENTABLE, not merely validated. The fields are private and the only
// constructor takes the reserved marker by value, so a struct literal that
// skips it — or one that supplies the sent marker directly — cannot compile.
use grapt_types::{DurableAttemptReceipt, RequestSentMarker, Timestamp};

fn main() {
    let _sent = RequestSentMarker::new(Timestamp::from_millis(1));
    // No reservation, and the fields are private in any case.
    let _receipt = DurableAttemptReceipt {
        request_sent: Some(_sent),
    };
}

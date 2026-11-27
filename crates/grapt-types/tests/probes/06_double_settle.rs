// Probe 6: settling a SettlementTicket twice must not compile —
// settle(self) consumes the ticket.
use grapt_types::{PermissionBlocker, PermissionReply, SettlementTicket};

fn main() {
    let ticket = SettlementTicket::<PermissionBlocker>::new(PermissionReply::Once);
    let _first = ticket.settle();
    let _second = ticket.settle();
}

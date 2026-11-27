// Probe 50 (R3a): a seat mailbox ADDRESS is its own identity. A recipient is
// not a Session, not an Agent and not a message id, and the compiler — not a
// review comment — is what refuses the substitutions.
//
// One error phase: every line below is E0308, so rustc reports all of them
// rather than stopping at an earlier resolution or privacy failure.
use grapt_types::{AgentId, MailboxMessageId, SeatRecipientId, SessionId};

fn requires_recipient(_: SeatRecipientId) {}
fn requires_message(_: MailboxMessageId) {}

fn main() {
    let session: SessionId = match "ses_1".parse() {
        Ok(id) => id,
        Err(_) => return,
    };
    let agent = match AgentId::new("agt-1") {
        Some(id) => id,
        None => return,
    };
    let recipient = match SeatRecipientId::new("seat-gamma") {
        Some(id) => id,
        None => return,
    };

    // A conversation is not an address.
    requires_recipient(session);
    // A governed actor is not an address either: two seats may run one actor,
    // and mail is delivered to the seat.
    requires_recipient(agent);
    // And an address is not a message.
    requires_message(recipient);
}

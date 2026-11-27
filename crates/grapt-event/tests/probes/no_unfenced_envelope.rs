//! Law: no new v1 (unfenced) write is nameable. The v2 constructor takes a
//! `WriterEpoch`; `EventFencing` — the enum whose `LegacyUnfenced` arm exists
//! only as a decode result — cannot be handed to it.
//!
//! Phase honesty: every name below resolves and the call is otherwise
//! well-formed, so the FIRST error is the type mismatch at the fencing
//! argument (E0308). This witnesses "the parameter is an epoch, not a
//! fencing state" — a stronger fact than a missing symbol, which would only
//! witness a name.

use grapt_event::{AggregateId, EnvelopeKind, EventEnvelopeV2, EventKind};
use grapt_types::{EventFencing, EventId, EventSeq, Timestamp};
use std::str::FromStr;

fn main() {
    let id = match EventId::from_str("evt_probe") {
        Ok(id) => id,
        Err(_) => return,
    };
    let aggregate = match "ses_probe".parse() {
        Ok(session) => AggregateId::Session(session),
        Err(_) => return,
    };
    let _ = EventEnvelopeV2::new(
        id,
        aggregate,
        EventSeq::FIRST,
        EventFencing::LegacyUnfenced,
        EnvelopeKind::Legacy(EventKind::SessionInputAdmittedV1),
        Timestamp::from_millis(0),
        Vec::new(),
    );
}

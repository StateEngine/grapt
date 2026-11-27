//! Governed append for one Rust-native `session.tool.terminal.1` fact.
//!
//! Artifact capture is a separate recovery unit and must already have committed
//! before the caller constructs the fact. This synchronous seam owns only the
//! event transaction. Stable caller-supplied identity, time, and fenced writer
//! make exact retries idempotent and changed-payload reuse divergent through
//! the accepted append chokepoint.

use crate::governed::GovernedTransaction;
use crate::write::{AppendInput, Appended, FencedWriter, Projector, WriteError};
use grapt_event::session_tool_terminal::{self, ToolTerminalFact};
use grapt_event::AggregateId;
use grapt_types::{EventId, EventVersion, SessionId};

const TOOL_TERMINAL: &str = "session.tool.terminal";

pub fn record(
    tx: &GovernedTransaction<'_>,
    session: &SessionId,
    id: &EventId,
    writer: FencedWriter,
    created_ms: i64,
    fact: &ToolTerminalFact,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    let aggregate = AggregateId::Session(session.clone());
    let payload = session_tool_terminal::encode(fact);
    let version = match EventVersion::new(1) {
        Some(version) => version,
        None => unreachable!("1 is a non-zero version"),
    };
    crate::EventStore::append_within(
        tx,
        AppendInput {
            id,
            aggregate,
            writer,
            kind_name: TOOL_TERMINAL,
            version,
            created_ms,
            data: &payload,
        },
        projectors,
    )
}

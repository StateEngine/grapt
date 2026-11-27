//! A resident client observes the existing Session; it never admits another prompt.

use std::io::Write;

use grapt_types::SessionId;

use super::{jsonl::JsonlMessages, ProgressBound, RunError, Runner, TERMINAL_POLL_INTERVAL};
use crate::{client::AdvisoryStream, AutoReply, BoundedOutput};

pub(super) async fn follow<O: Write, E: Write>(
    runner: &Runner,
    session: &SessionId,
    mut events: AdvisoryStream,
    output: &mut BoundedOutput<O, E>,
    mut messages_output: JsonlMessages,
    mut last_permissions: Option<Vec<u8>>,
) -> Result<(), RunError> {
    let mut poll = tokio::time::interval(TERMINAL_POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut reconnects = ProgressBound::default();
    loop {
        tokio::select! {
            event = events.next() => {
                // Drain the existing bounded advisory transport to keep its
                // authenticated approver attachment alive. Durable polls own output.
                if event.is_err() {
                    reconnects.reconnect()?;
                    events = runner.client.events_with_approver(
                        (runner.auto != AutoReply::Off).then_some(session),
                    ).await?;
                }
            }
            _ = poll.tick() => {
                // An idle seat need not change durable state. Successful reads
                // reset consecutive stream failures, allowing routine renewal
                // throughout a seat-day without an unbounded reconnect burst.
                let messages = runner.client.messages(session).await?;
                messages_output.emit(session, &messages, false, output)?;
                runner.emit_changed_permissions(session, &mut last_permissions, output).await?;
                reconnects.observe(true);
            }
        }
    }
}

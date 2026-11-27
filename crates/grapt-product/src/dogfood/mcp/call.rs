//! Admission, fencing and retirement of one configured MCP tool call.
use super::{
    channel, describe_launch, run_call_with, LaunchIdentity, McpCallRefusal, McpRegistry,
    ServerEntry, MAX_QUEUED_CALLERS,
};
use grapt_mcp::{session::Unusable, McpToolName, ToolCallOutcome};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

const MAX_CONNECTION_BINDS: usize = 2;

/// A private exchange abandoned mid-frame cannot lend its channel to the next
/// call: the unconsumed reply still belongs to the abandoned request.
struct PrivateExchange<'a, T> {
    held: &'a channel::HeldIo<T>,
    completed: bool,
}
impl<T> Drop for PrivateExchange<'_, T> {
    fn drop(&mut self) {
        if !self.completed {
            self.held.observe(Unusable::Failed(
                "private MCP exchange interrupted".to_owned(),
            ));
            self.held.retire();
        }
    }
}

impl McpRegistry {
    /// Run one discovered tool on one connected server, under this call's own
    /// lease.
    ///
    /// The lease is what makes the hard cases decidable rather than hopeful.
    /// `begin_call` refuses a tool this connection never discovered, a call id
    /// already in flight, a call id that already reached a terminal, and a load
    /// above the declared bound — before a byte is written. `settle` refuses a
    /// lease minted under a retired generation. A grant binds the declaration;
    /// each actual dispatch still uses a fresh lease on its current connection.
    ///
    /// The connection is addressed by LOCATION and NAME together, and the tool
    /// by the name this connection actually published. A call cannot be
    /// redirected to a same-named server in another project.
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a distinct call coordinate; grouping them would only move the same list"
    )]
    #[cfg(test)]
    pub(in super::super) async fn call(
        &self,
        location: &str,
        server: &str,
        tool: &McpToolName,
        approved: &LaunchIdentity,
        call_id: &str,
        arguments: Value,
        deadline: Duration,
    ) -> Result<ToolCallOutcome, McpCallRefusal> {
        self.call_with(
            location, server, tool, approved, call_id, arguments, deadline, None, None, None,
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "existing call coordinates plus the nested request consumer"
    )]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(in super::super) async fn call_with(
        &self,
        location: &str,
        server: &str,
        tool: &McpToolName,
        approved: &LaunchIdentity,
        call_id: &str,
        arguments: Value,
        deadline: Duration,
        mut handler: Option<&mut dyn channel::ServerRequestHandler>,
        attempt: Option<&grapt_types::PhysicalAttemptId>,
        metadata: Option<Value>,
    ) -> Result<ToolCallOutcome, McpCallRefusal> {
        // The registry lock is held ONLY to find the connection and clone its
        // I/O handle. It is released before any network work, so a silent
        // server cannot block another location's status read or an unrelated
        // dispatch.
        // THE BUDGET COVERS THE WHOLE ADMISSION. The clock starts before the
        // registry lock, not after it: a caller queued behind a status refresh
        // was previously charged nothing for that wait and could outlive the
        // deadline it was given before its own work began.
        let mut admitted = tokio::time::Instant::now();
        let bound_producer = server == "stateengine" && metadata.is_some();
        let retry_read =
            bound_producer && super::producer_version::replayable_read(tool, &arguments);
        let mut arguments = arguments;
        let mut retry_send = false;
        let mut replayed = false;
        for binding in 0..MAX_CONNECTION_BINDS {
            if binding > 0 {
                let human_paused = super::admission::recover_queued(
                    self,
                    location,
                    server,
                    approved,
                    deadline.saturating_sub(admitted.elapsed()),
                )
                .await?;
                admitted = admitted
                    .checked_add(human_paused)
                    .ok_or(McpCallRefusal::TimedOut)?;
            }
            let io = {
                let Ok(mut locations) = tokio::time::timeout(
                    deadline.saturating_sub(admitted.elapsed()),
                    self.entries.lock(),
                )
                .await
                else {
                    return Err(McpCallRefusal::TimedOut);
                };
                // A MISSING LOCATION AND A MISSING SERVER ARE DIFFERENT FAULTS.
                // Reporting both as `NoSuchServer` sent a location-keying defect
                // to be investigated as a publication one.
                let held = locations.get_mut(location).ok_or_else(|| {
                    if binding == 0 {
                        McpCallRefusal::NoSuchLocation(location.to_owned())
                    } else {
                        retired_while_queued(approved)
                    }
                })?;
                let entry = held.entries.get_mut(server).ok_or_else(|| {
                    if binding == 0 {
                        McpCallRefusal::NoSuchServer(server.to_owned())
                    } else {
                        retired_while_queued(approved)
                    }
                })?;
                let live = match entry {
                    ServerEntry::Live(live) => live,
                    ServerEntry::Disabled => {
                        return Err(McpCallRefusal::NotConnected(Unusable::Disabled))
                    }
                    ServerEntry::Unusable(reason, _, _) => {
                        return Err(McpCallRefusal::NotConnected(reason.clone()))
                    }
                };

                // Reconnection alone keeps the grant. Configuration and the
                // resolved launch must still match before taking any I/O handle.
                if !live.launch().matches_approval(approved) {
                    return Err(McpCallRefusal::ConnectionReplaced {
                        approved: describe_launch(approved),
                        current: describe_launch(live.launch()),
                    });
                }
                (
                    Arc::clone(&live.io),
                    live.stderr.clone(),
                    super::producer_version::accepts_send_key(tool, live.catalog()),
                )
            };
            if bound_producer && binding == 0 && io.2 {
                retry_send = super::producer_version::prepare_send(
                    &mut arguments,
                    location,
                    call_id,
                    attempt,
                );
            } else if retry_send && !io.2 {
                // A replacement that no longer advertises the contract cannot
                // receive an uncertain write, even with its original key.
                return Err(McpCallRefusal::ProducerVersionChanged { recovered: true });
            }
            if metadata.is_some() {
                if let Some(capture) = &io.1 {
                    for value in arguments
                        .as_object()
                        .into_iter()
                        .flat_map(|values| values.values())
                        .chain(
                            metadata
                                .as_ref()
                                .and_then(Value::as_object)
                                .into_iter()
                                .flat_map(|values| values.values()),
                        )
                        .filter_map(Value::as_str)
                    {
                        capture.protect(value);
                    }
                }
            }
            let held = io.0;

            // WAITING IS INSIDE THE BUDGET. One caller at a time per connection is
            // a real constraint — the channel is a single correlated stream — but
            // a caller queued behind another call must not wait past its own
            // deadline and then still start. The lock wait is covered by the same
            // deadline as the exchange, so a caller times out rather than
            // silently outliving the budget it was given.
            // ADMITTED TO THE QUEUE, or refused. A deadline bounds how LONG one
            // caller waits, not how MANY do, and each waiter holds a reference
            // that keeps the child alive past any eviction. The ticket is released
            // however this caller leaves, including by timing out.
            let Some(_ticket) = held.enqueue(MAX_QUEUED_CALLERS) else {
                return Err(McpCallRefusal::Busy {
                    waiting: MAX_QUEUED_CALLERS,
                });
            };
            let remaining = deadline.saturating_sub(admitted.elapsed());
            let mut guard = tokio::select! {
                biased;
                () = held.retired() => continue,
                acquired = tokio::time::timeout(remaining, held.io().lock()) => acquired,
            }
            .map_err(|_| McpCallRefusal::TimedOut)?;
            // Nothing was written while queued. Resolve the current connection
            // again if this one retired, under the original deadline and grant.
            if held.is_retired() {
                continue;
            }
            let remaining = deadline.saturating_sub(admitted.elapsed());
            let io = &mut *guard;
            let mut private_exchange = metadata.as_ref().map(|_| PrivateExchange {
                held: held.as_ref(),
                completed: false,
            });
            let mut channel_reusable = false;
            // Only bound StateEngine reads and a send keyed before its first
            // attempt may replay, and only after an explicit producer refusal.
            let outcome = tokio::select! {
                biased;
                () = held.retired() => Err(McpCallRefusal::ConnectionReplaced {
                    approved: describe_launch(approved), current: "connection retired during the call".into(),
                }),
                outcome = run_call_with(
                &mut io.channel,
                &mut io.session,
                self.bounds,
                tool,
                call_id,
                arguments.clone(),
                remaining,
                handler.as_mut().map(|consumer| &mut **consumer as &mut dyn channel::ServerRequestHandler),
                attempt,
                metadata.clone(),
                &mut channel_reusable,
                ) => outcome,
            };
            // Validation of an already consumed result cannot poison its channel.
            if channel_reusable {
                if let Some(exchange) = &mut private_exchange {
                    exchange.completed = true;
                }
            }
            // A hangup this call SAW is knowledge the next status read needs. It is
            // published without waiting on anything, so a list can report the
            // present rather than repeating that startup once succeeded.
            if let Err(McpCallRefusal::Disconnected(_)) = &outcome {
                held.observe(Unusable::Disconnected);
            }
            if bound_producer && super::producer_version::refused(&outcome) {
                // A refusal cannot prove that a write had no effect. A keyed
                // send instead relies on the server's advertised deduplication.
                // Unsent callers retain ordinary bounded queued recovery.
                held.observe(Unusable::Disconnected);
                held.retire();
                if let Some(exchange) = &mut private_exchange {
                    exchange.completed = true;
                }
                drop(private_exchange);
                drop(guard);
                drop(_ticket);
                drop(held);
                self.changed.notify_waiters();
                let retryable = retry_read || retry_send;
                let replay = retryable && binding + 1 < MAX_CONNECTION_BINDS;
                let recovered = if retryable && !replay {
                    false
                } else {
                    match super::admission::recover_queued(
                        self,
                        location,
                        server,
                        approved,
                        deadline.saturating_sub(admitted.elapsed()),
                    )
                    .await
                    {
                        Ok(human_paused) => {
                            admitted = admitted
                                .checked_add(human_paused)
                                .ok_or(McpCallRefusal::TimedOut)?;
                            true
                        }
                        Err(_) => false,
                    }
                };
                if replay && recovered {
                    replayed = true;
                    continue;
                }
                return Err(if replayed && retry_send {
                    McpCallRefusal::ProducerWriteRetryRefused
                } else if replayed {
                    McpCallRefusal::ProducerReadRetryRefused
                } else {
                    McpCallRefusal::ProducerVersionChanged { recovered }
                });
            }
            return outcome;
        }
        Err(retired_while_queued(approved))
    }
}

fn retired_while_queued(approved: &LaunchIdentity) -> McpCallRefusal {
    McpCallRefusal::ConnectionReplaced {
        approved: describe_launch(approved),
        current: "a connection retired while this call was queued".to_owned(),
    }
}

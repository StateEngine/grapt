use super::*;
use std::task::Poll;

const MAX_COALESCED_READY_FRAGMENTS: usize = 64;

/// The one reader task. Cancellable directly, and it measures its own
/// backpressure so the owner does not have to guess.
///
/// Every wait on `cancel` treats a *closed* channel as terminal. A closed
/// `watch` is permanently ready, so a biased branch that looped on it would
/// spin without ever polling the reader again — a live task burning a core and
/// holding the stream open. Losing every sender means the attempt frame that
/// owns this reader is gone, which is the strongest possible reason to stop.
pub(super) async fn pump_reader<R: ProviderReader>(
    mut reader: R,
    tx: mpsc::Sender<AttemptFragment>,
    mut cancel: watch::Receiver<Option<AttemptCancelCause>>,
    mut step: watch::Receiver<Option<InterruptionSource>>,
    shared: Arc<SharedDiagnostics>,
) {
    // Three facts, never one rule.
    //   * A Step cancel already published stops this reader. The supervisor
    //     holds the same source and names the typed cause when the stream
    //     closes, so the attempt settles as interrupted rather than as an
    //     ordinary end, without this task keeping the attempt channel alive.
    //   * A closed Step watch only removes that interrupt source. The Step can
    //     no longer interrupt; it has not ended, and the attempt stays healthy.
    //     This matches the driver, which keeps running when its own sender goes.
    //   * A closed attempt channel means this reader's owner is gone. That is
    //     the one sender loss that stops the reader, and it is unchanged.
    let mut step_available = true;
    let mut pending: Option<AttemptFragment> = None;
    let mut coalesced_ready = 0;
    loop {
        if cancel.has_changed().is_err() || cancel.borrow().is_some() {
            return;
        }
        if step_stopped(&mut step, &mut step_available) {
            return;
        }
        let mut delivery = Delivery {
            tx: &tx,
            cancel: &mut cancel,
            step: &mut step,
            step_available: &mut step_available,
            pending: &mut pending,
            coalesced_ready: &mut coalesced_ready,
            shared: &shared,
        };
        let fragment = match read_next(&mut reader, &mut delivery).await {
            Ok(ReadStep::Fragment(fragment)) => *fragment,
            Err(Stopped) => return,
        };
        let Some(mut fragment) = fragment else {
            if let Some(pending) = delivery.pending.take() {
                let _abandoned = send_fragment(&mut delivery, pending).await;
            }
            return;
        };
        match coalesce(&mut delivery, fragment).await {
            Ok(Some(next)) => fragment = next,
            Ok(None) => continue,
            Err(Stopped) => return,
        }
        if fragment.is_coalescible_delta() {
            pending = Some(fragment);
            continue;
        }
        if send_fragment(&mut delivery, fragment).await == SendOutcome::Stopped {
            return;
        }
    }
}

/// Ends a read or a send: the attempt stops here.
#[derive(Debug)]
struct Stopped;

/// The channels and diagnostics one reader delivers through.
struct Delivery<'a> {
    tx: &'a mpsc::Sender<AttemptFragment>,
    cancel: &'a mut watch::Receiver<Option<AttemptCancelCause>>,
    step: &'a mut watch::Receiver<Option<InterruptionSource>>,
    step_available: &'a mut bool,
    pending: &'a mut Option<AttemptFragment>,
    coalesced_ready: &'a mut usize,
    shared: &'a SharedDiagnostics,
}

/// What one read produced. A read is retained until it completes, so there is
/// no "try again" outcome: only a fragment or a stop.
enum ReadStep {
    Fragment(Box<Option<AttemptFragment>>),
}

/// Read the next fragment, or flush the coalesced one first when output is
/// pending. Cancellation outranks provider I/O at every wait.
///
/// One `next()` operation owns exactly one pinned read. `next()` returns a
/// single fragment, so a completed read cannot be polled again; a new read
/// after a completed fragment is necessary and correct. What must never happen
/// is dropping a read that has not completed: `AttemptReader::next_fragment`
/// parks its state as `Finished` on entry, so a dropped read would end the
/// reader and report an unclean provider end. The inner loop therefore holds
/// this operation's pinned read across every harmless wake and only returns
/// when the read completes or a real cancellation arrives.
async fn read_next<R: ProviderReader>(
    reader: &mut R,
    delivery: &mut Delivery<'_>,
) -> Result<ReadStep, Stopped> {
    let mut next = std::pin::pin!(reader.next());
    // Coalesce a ready burst, but never wait for provider I/O while holding
    // visible output. Keep the SAME next future alive across this flush;
    // coalescing never discards a partial transport read.
    let immediate = if delivery.pending.is_some() {
        std::future::poll_fn(|cx| Poll::Ready(next.as_mut().poll(cx))).await
    } else {
        Poll::Pending
    };
    match immediate {
        Poll::Ready(fragment) => return Ok(ReadStep::Fragment(Box::new(fragment))),
        Poll::Pending => {}
    }
    if let Some(held) = delivery.pending.take() {
        if send_fragment(delivery, held).await == SendOutcome::Stopped {
            return Err(Stopped);
        }
        *delivery.coalesced_ready = 0;
    }
    // Retain this operation's read across harmless wakes. A wake that is not a
    // cancellation, and a Step sender closing, change nothing about the read.
    loop {
        tokio::select! {
            biased;
            changed = delivery.cancel.changed() => {
                // A closed attempt channel means this reader's owner is gone.
                if changed.is_err() || delivery.cancel.borrow().is_some() {
                    return Err(Stopped);
                }
                continue;
            }
            changed = delivery.step.changed(), if *delivery.step_available => {
                match changed {
                    // A published cancel wins even if its sender has since
                    // closed: read the current value first.
                    Ok(()) => {
                        if step_stopped(delivery.step, delivery.step_available) {
                            return Err(Stopped);
                        }
                        // A non-cancelling or duplicate wake leaves the branch
                        // enabled for a later cancellation.
                        continue;
                    }
                    // Only a closed sender disables the source.
                    Err(_) => {
                        *delivery.step_available = false;
                        continue;
                    }
                }
            }
            fragment = &mut next => return Ok(ReadStep::Fragment(Box::new(fragment))),
        }
    }
}

/// Fold the newly read fragment into the coalesced one.
async fn coalesce(
    delivery: &mut Delivery<'_>,
    mut fragment: AttemptFragment,
) -> Result<Option<AttemptFragment>, Stopped> {
    let Some(mut current) = delivery.pending.take() else {
        return Ok(Some(fragment));
    };
    match current.coalesce_or_return(fragment) {
        None => {
            delivery
                .shared
                .provider_fragments_coalesced
                .fetch_add(1, Ordering::Relaxed);
            *delivery.coalesced_ready += 1;
            if *delivery.coalesced_ready >= MAX_COALESCED_READY_FRAGMENTS {
                if send_fragment(delivery, current).await == SendOutcome::Stopped {
                    return Err(Stopped);
                }
                *delivery.coalesced_ready = 0;
            } else {
                *delivery.pending = Some(current);
            }
            Ok(None)
        }
        Some(next) => {
            if send_fragment(delivery, current).await == SendOutcome::Stopped {
                return Err(Stopped);
            }
            *delivery.coalesced_ready = 0;
            fragment = next;
            Ok(Some(fragment))
        }
    }
}

/// Whether the Step source says this reader must stop.
///
/// A published cancel stops it. A closed watch only removes the source: the
/// Step cannot interrupt any more, which is not the same fact as a Step that
/// was cancelled, so the attempt keeps running.
fn step_stopped(
    step: &mut watch::Receiver<Option<InterruptionSource>>,
    available: &mut bool,
) -> bool {
    // Read the CURRENT value, not the notification: a cancel that was already
    // observed, or one retained after every sender closed, still stops reading.
    let cancelled = step.borrow_and_update().is_some();
    if step.has_changed().is_err() {
        // Closed watch: only the interrupt source is gone, never the value that
        // was already published.
        *available = false;
    }
    cancelled
}

/// Whether a fragment reached the owner or the send was abandoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendOutcome {
    Delivered,
    Stopped,
}

async fn send_fragment(delivery: &mut Delivery<'_>, fragment: AttemptFragment) -> SendOutcome {
    let tx = delivery.tx;
    // Fast path first. Timing every reserve would count ordinary scheduler
    // latency as backpressure, so a non-zero wait would prove nothing.
    // Only a genuinely FULL channel counts as contention.
    let permit = match tx.try_reserve() {
        Ok(permit) => permit,
        Err(mpsc::error::TrySendError::Closed(())) => return SendOutcome::Stopped,
        Err(mpsc::error::TrySendError::Full(())) => {
            delivery
                .shared
                .provider_contention_events
                .fetch_add(1, Ordering::Relaxed);
            let started = tokio::time::Instant::now();
            // One reservation, pinned once: a harmless wake must not abandon
            // it, because a reservation is this fragment's place in the queue
            // and re-creating it would send us to the back.
            let mut reserving = std::pin::pin!(tx.reserve());
            let permit = loop {
                tokio::select! {
                    biased;
                    changed = delivery.cancel.changed() => {
                        if changed.is_err() || delivery.cancel.borrow().is_some() {
                            return SendOutcome::Stopped;
                        }
                        continue;
                    }
                    changed = delivery.step.changed(), if *delivery.step_available => {
                        match changed {
                            // A Step cancel outranks a place in the queue: the
                            // fragment is abandoned, not delivered.
                            Ok(()) => {
                                if step_stopped(delivery.step, delivery.step_available) {
                                    return SendOutcome::Stopped;
                                }
                            }
                            // Only a closed sender disables the source.
                            Err(_) => *delivery.step_available = false,
                        }
                        continue;
                    }
                    permit = &mut reserving => break permit,
                }
            };
            delivery
                .shared
                .producer_wait_nanos
                .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            let Ok(permit) = permit else {
                return SendOutcome::Stopped;
            };
            permit
        }
    };
    permit.send(fragment);

    let depth = tx.max_capacity().saturating_sub(tx.capacity());
    delivery
        .shared
        .provider_high_water
        .fetch_max(depth, Ordering::Relaxed);
    SendOutcome::Delivered
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_tool<T: ToolRunner>(
    tasks: &mut JoinSet<()>,
    tools: Arc<T>,
    permits: Arc<Semaphore>,
    shared: Arc<SharedDiagnostics>,
    tool_tx: mpsc::Sender<ToolMessage>,
    mut cancel: watch::Receiver<Option<AttemptCancelCause>>,
    call: ToolCallId,
    tool: String,
    input: crate::tool_protocol::BoundedToolInput,
    residency: Residency,
    // The attempt that admitted this call, from the loop that owns it. Carried
    // so the executor can name WHICH dispatch raised a nested effect; the
    // dispatch path never had it before.
    attempt: Option<grapt_types::PhysicalAttemptId>,
    nested: Option<NestedTools>,
) {
    tasks.spawn(async move {
        // Held for the task's whole life. A `JoinSet` aborts its tasks on drop,
        // but an abort is a request; this token is what proves it landed.
        let _residency = residency;
        // The budget bounds concurrency. The 17th tool waits here.
        let admission = nested.as_ref().map(NestedTools::composition_permits)
            .unwrap_or_else(|| Arc::clone(&permits));
        if admission.available_permits() == 0 {
            shared.tool_permit_waits.fetch_add(1, Ordering::Relaxed);
        }
        let permit = tokio::select! {
            biased;
            _ = cancel.changed() => {
                if cancel.borrow().is_some() { return }
                admission.acquire().await
            }
            permit = admission.acquire() => permit,
        };
        let Ok(_permit) = permit else { return };

        let running = shared.tools_running.fetch_add(1, Ordering::Relaxed) + 1;
        shared
            .tools_peak_concurrent
            .fetch_max(running, Ordering::Relaxed);

        let cancellation_input = input.clone();
        let outcome = tokio::select! {
            biased;
            _ = cancel.changed() => {
                if let Some(cause) = *cancel.borrow() {
                    shared.tools_running.fetch_sub(1, Ordering::Relaxed);
                    let by = match cause {
                        AttemptCancelCause::Interrupted { by } => by,
                        AttemptCancelCause::NoProgress => InterruptionSource::ManagedShutdown,
                    };
                    let _ = tool_tx.try_send(ToolMessage::Terminal {
                        call,
                        outcome: cancelled_tool_outcome(by),
                    });
                    return;
                }
                tools
                    .run_with_nested(attempt.clone(), call.clone(), tool, cancellation_input, nested.clone())
                    .await
            }
            outcome = tools.run_with_nested(
                attempt.clone(), call.clone(), tool.clone(), input, nested.clone()) => outcome,
        };
        shared.tools_running.fetch_sub(1, Ordering::Relaxed);
        let _ = tool_tx.send(ToolMessage::Terminal { call, outcome }).await;
    });
}

/// The single cancellation-terminal seam. Two independent producers can settle
/// a cancelled tool — the in-flight cancel arm above and attempt_loop's
/// no-terminal fallback — so both must construct their terminal here.
pub(super) fn cancelled_tool_outcome(by: InterruptionSource) -> ToolOutcome {
    ToolOutcome::Cancelled { by }
}

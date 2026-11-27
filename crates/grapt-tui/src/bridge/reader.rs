//! The reader thread: one blocking event stream, the replacements it builds
//! when the backend asks, and the per-frame refreshes it raises.
use super::*;

/// One replacement that passed its checks but is not installed yet.
///
/// The two connections are deliberately named: `rpc` carries commands and
/// `stream` carries events, and a swap of the two would still compile.
struct Built {
    rpc: HttpConnection,
    stream: HttpConnection,
    cancel: Option<StreamCancellation>,
}

/// Open one replacement subscription, naming the Session watched right now.
///
/// `Ok(None)` means the window selected another Session while this handshake
/// was in flight. The built connection is DROPPED, which closes its socket and
/// withdraws the lease it took for the Session the window left; adopting it
/// would keep serving a stale lease while the Session now shown has none.
fn build_replacement(
    config: &Config,
    bounds: Bounds,
    session: &Arc<std::sync::Mutex<WatchedSessions>>,
    expected: u64,
    shared: &AtomicU64,
    legacy: bool,
    offer: Option<grapt_client::client::ResumeToken>,
) -> Result<Option<Built>, String> {
    let (rpc, mut next) = config.connect(bounds)?;
    // Every replacement adopts the Sessions this window watches, so a
    // reconnect inside the grace keeps every claim instead of starting fresh.
    next.adopt_watched(current_watched(session));
    if legacy {
        next.refuse_positions();
    }
    // The subscribe carries the client's offer only under the token's binding.
    next.resume_from(offer);
    next.resubscribe();
    if !next.subscribed() {
        // The subscribe's own refusal (a 401, say), never a vague failure.
        return Err(next.poll_frame().err().map_or_else(
            || "service event subscription unavailable".into(),
            |refusal| refusal.to_string(),
        ));
    }
    if shared.load(Ordering::Acquire) != expected {
        return Ok(None);
    }
    let cancel = next.stream_cancellation().map_err(|e| e.to_string())?;
    Ok(Some(Built {
        rpc,
        stream: next,
        cancel,
    }))
}

/// What the reader does after one replacement attempt.
enum Replacement {
    /// A verified attachment is installed; read from it.
    Read,
    /// Nothing was installed: wait for the backend to ask again.
    Retry,
    /// A handoff closed, so this reader is finished.
    Stop,
}

/// The reader's shared outputs, so one replacement attempt stays one call.
pub(super) struct Reader {
    pub(super) config: Config,
    pub(super) bounds: Bounds,
    pub(super) session: Arc<std::sync::Mutex<WatchedSessions>>,
    pub(super) generation: Arc<AtomicU64>,
    pub(super) legacy: Arc<AtomicBool>,
    pub(super) stream: HttpConnection,
    pub(super) valid: Arc<AtomicBool>,
    pub(super) handoff: SyncSender<Delivery>,
    pub(super) wake: SyncSender<()>,
    pub(super) custody: SharedCustody,
    pub(super) suppress: Suppress,
    pub(super) retry: Receiver<()>,
    pub(super) gap: SyncSender<()>,
    pub(super) authority: Arc<AtomicBool>,
    pub(super) sessions: Arc<AtomicBool>,
}

/// Open one replacement and hand its barrier to the backend.
fn deliver_replacement(reader: &mut Reader) -> Replacement {
    let built = reader.generation.load(Ordering::Acquire);
    let (offer, suffix) = resume::offer(&reader.custody);
    let result = build_replacement(
        &reader.config,
        reader.bounds,
        &reader.session,
        built,
        &reader.generation,
        reader.legacy.load(Ordering::Acquire),
        offer.clone(),
    );
    let (result, next_step) = match result {
        Ok(Some(Built {
            rpc,
            stream,
            cancel,
        })) => {
            reader.stream = stream;
            reader.valid = Arc::new(AtomicBool::new(true));
            reader.suppress = resume::record(
                &reader.custody,
                &reader.valid,
                &reader.stream,
                offer,
                suffix,
            );
            (
                Ok(Box::new(Attachment {
                    rpc,
                    cancel,
                    valid: reader.valid.clone(),
                })),
                Replacement::Read,
            )
        }
        // A stale build is still reported, or the backend's barrier never
        // arrives and it waits forever. `Stale` hands the decision back to build
        // again for the Session selected now.
        Ok(None) => {
            if reader.handoff.send(Delivery::Stale).is_err() {
                return Replacement::Stop;
            }
            let _ = reader.wake.try_send(());
            return Replacement::Retry;
        }
        Err(reason) => (Err(reason), Replacement::Retry),
    };
    if reader
        .handoff
        .send(Delivery::Replacement(result, built))
        .is_err()
    {
        return Replacement::Stop;
    }
    let _ = reader.wake.try_send(());
    next_step
}

/// Read until the bridge goes away: each stream until it gaps or closes,
/// then the replacement the backend asks for.
pub(super) fn read_events(mut reader: Reader) {
    // Read fresh at each reconnect: a window that switched Sessions
    // must not reopen the previous one's attachment.
    let mut reconnect_needed = !reader.stream.subscribed();
    loop {
        if reconnect_needed {
            if reader.retry.recv().is_err() {
                return;
            }
            match deliver_replacement(&mut reader) {
                Replacement::Read => {}
                Replacement::Retry => continue,
                Replacement::Stop => return,
            }
        }
        // Read until the stream gaps or closes; either way the
        // next pass waits for the backend to ask for a replacement.
        reconnect_needed = true;
        loop {
            let frame = match reader.stream.wait_frame() {
                Ok(frame) => frame,
                Err(_) => FramePoll::Closed,
            };
            // A gap loses live events, not the service: the RPC
            // attachment stays valid so input and stop keep working
            // while the view resynchronises.
            if frame == FramePoll::Gap {
                if reader.gap.send(()).is_err() {
                    return;
                }
                let _ = reader.wake.try_send(());
                break;
            }
            if frame == FramePoll::Closed {
                // Preserve the live-only prefix, including a stop reason.
                reader.valid.store(false, Ordering::Release);
                if reader.handoff.send(Delivery::Frame(frame)).is_err() {
                    return;
                }
                let _ = reader.wake.try_send(());
                break;
            }
            let (id, text) = match &frame {
                FramePoll::Frame(text) => (None, Some(text)),
                FramePoll::Positioned { id, data } => (Some(id.as_str()), Some(data)),
                _ => (None, None),
            };
            if let Some(text) = text {
                let (permissions, catalog) = resume::frame_flags(id, text, &mut reader.suppress);
                if permissions {
                    reader.authority.store(true, Ordering::Release);
                }
                if catalog {
                    reader.sessions.store(true, Ordering::Release);
                }
            }
            // Wait for room rather than drop: a view that falls
            // behind holds the stream back. Only the bridge going
            // away ends the wait.
            if reader.handoff.send(Delivery::Frame(frame)).is_err() {
                reader.valid.store(false, Ordering::Release);
                return;
            }
            let _ = reader.wake.try_send(());
        }
    }
}

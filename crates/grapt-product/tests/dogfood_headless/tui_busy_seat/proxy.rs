//! A loopback relay between the TUI and the service. It forwards bytes
//! unchanged, and on demand cuts every open connection (a live-stream gap the
//! service never chose), or stops listening while the service restarts. Pairs
//! whose pumps have both ended and released their ends drop at the next reap,
//! so a long session cannot exhaust the process's fd budget.
use super::*;

#[path = "proxy/trace.rs"]
mod trace;

#[derive(Default)]
struct Relayed {
    upstream: Option<SocketAddr>,
    open: Vec<OpenConnection>,
    /// Added before each chunk from the service reaches the TUI.
    delay: Duration,
    /// Event-stream subscriptions the TUI has sent so far.
    subscriptions: usize,
    trace: Option<Vec<String>>,
    trace_changed: Arc<Condvar>,
}

/// One relayed connection: both retained ends plus each pump's completion
/// flag, so a pair whose pumps have both ended and released their ends drops
/// at the next reap instead of holding its fds until the next cut.
struct OpenConnection {
    client: TcpStream,
    server: TcpStream,
    up: Arc<AtomicBool>,
    down: Arc<AtomicBool>,
}

impl Relayed {
    /// Drops entries whose pumps have both ended and released their ends: no
    /// pump can still hold an end of a dropped pair, so the drop is the pair's
    /// last close.
    fn retain_live(&mut self) {
        self.open
            .retain(|pair| !(pair.up.load(Ordering::SeqCst) && pair.down.load(Ordering::SeqCst)));
    }
}

pub(super) struct Relay {
    pub(super) address: SocketAddr,
    state: Arc<Mutex<Relayed>>,
    stop: Arc<AtomicBool>,
    acceptor: Option<thread::JoinHandle<()>>,
}

impl Relay {
    pub(super) fn start(upstream: SocketAddr) -> Self {
        let state = Arc::new(Mutex::new(Relayed {
            upstream: Some(upstream),
            open: Vec::new(),
            delay: Duration::ZERO,
            subscriptions: 0,
            trace: None,
            trace_changed: Arc::new(Condvar::new()),
        }));
        let listener = TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|error| panic!("relay listen failed: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("relay address absent: {error}"));
        let mut relay = Self {
            address,
            state,
            stop: Arc::new(AtomicBool::new(false)),
            acceptor: None,
        };
        relay.serve(listener);
        relay
    }

    fn serve(&mut self, listener: TcpListener) {
        listener
            .set_nonblocking(true)
            .unwrap_or_else(|error| panic!("relay listener mode failed: {error}"));
        self.stop = Arc::new(AtomicBool::new(false));
        let (state, stop) = (self.state.clone(), self.stop.clone());
        self.acceptor = Some(
            thread::Builder::new()
                .name("busy-seat-relay".into())
                .spawn(move || accept(&listener, &state, &stop))
                .unwrap_or_else(|error| panic!("relay failed to start: {error}")),
        );
    }

    /// Event-stream subscriptions the TUI has sent through the relay so far.
    pub(super) fn subscriptions(&self) -> usize {
        locked(&self.state).subscriptions
    }

    pub(super) fn trace_start(&self) {
        locked(&self.state).trace = Some(Vec::new());
    }

    pub(super) fn requests(&self) -> Vec<String> {
        locked(&self.state)
            .trace
            .clone()
            .unwrap_or_else(|| panic!("relay request trace was not started"))
    }

    /// Waits for the requested trace evidence, or returns the trace at the deadline.
    pub(super) fn requests_until(
        &self,
        within: Duration,
        complete: impl Fn(&[String]) -> bool,
    ) -> Vec<String> {
        let deadline = Instant::now() + within;
        let mut state = locked(&self.state);
        let changed = state.trace_changed.clone();
        loop {
            let trace = state
                .trace
                .as_ref()
                .unwrap_or_else(|| panic!("relay request trace was not started"));
            if complete(trace) || Instant::now() >= deadline {
                return trace.clone();
            }
            state = match changed
                .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
            {
                Ok((state, _)) => state,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// Closes every live relayed connection in both directions at once;
    /// pairs whose pumps have already ended and released their ends are
    /// dropped, not cut.
    pub(super) fn cut(&self) -> usize {
        let mut state = locked(&self.state);
        state.retain_live();
        let open = std::mem::take(&mut state.open);
        for pair in &open {
            // An already closed side has nothing left to cut.
            let _ = pair.client.shutdown(std::net::Shutdown::Both);
            let _ = pair.server.shutdown(std::net::Shutdown::Both);
        }
        open.len()
    }

    /// Delays every chunk the service sends, as a slow or busy service would.
    pub(super) fn delay(&self, delay: Duration) {
        locked(&self.state).delay = delay;
    }

    /// Stops listening and cuts every connection: while the service is down,
    /// a connect is refused exactly as it is at a dead service's port.
    pub(super) fn suspend(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(acceptor) = self.acceptor.take() {
            if acceptor.join().is_err() {
                panic!("relay acceptor panicked");
            }
        }
        self.cut();
    }

    /// Listens again on the same address, relaying to the restarted service.
    pub(super) fn resume(&mut self, upstream: SocketAddr) {
        locked(&self.state).upstream = Some(upstream);
        let wait = PollDeadline::within(Duration::from_secs(10));
        let listener = loop {
            match TcpListener::bind(self.address) {
                Ok(listener) => break listener,
                Err(error) if wait.pending() => {
                    eprintln!("relay rebind retry: {error}");
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("relay could not listen again on {}: {error}", self.address),
            }
        };
        self.serve(listener);
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.cut();
        if let Some(acceptor) = self.acceptor.take() {
            if acceptor.join().is_err() && !thread::panicking() {
                panic!("relay acceptor panicked");
            }
        }
    }
}

fn accept(listener: &TcpListener, state: &Arc<Mutex<Relayed>>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((client, _)) => {
                let upstream = locked(state).upstream;
                let Some(server) = upstream.and_then(|address| TcpStream::connect(address).ok())
                else {
                    // The service is between generations: refuse, as it would.
                    drop(client);
                    continue;
                };
                // The accepted socket inherits the listener's mode; relays block.
                if client.set_nonblocking(false).is_err() {
                    continue;
                }
                let (Ok(client_copy), Ok(server_copy)) = (client.try_clone(), server.try_clone())
                else {
                    continue;
                };
                let (up, down) = (
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicBool::new(false)),
                );
                {
                    let mut state = locked(state);
                    state.open.push(OpenConnection {
                        client: client_copy,
                        server: server_copy,
                        up: up.clone(),
                        down: down.clone(),
                    });
                    state.retain_live();
                }
                relay_pair(client, server, state, up, down);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("relay accept failed: {error}"),
        }
    }
}

fn relay_pair(
    client: TcpStream,
    server: TcpStream,
    state: &Arc<Mutex<Relayed>>,
    up: Arc<AtomicBool>,
    down: Arc<AtomicBool>,
) {
    let (upstream, downstream) = (server.try_clone(), client.try_clone());
    if upstream.is_err() || downstream.is_err() {
        // Both directions are already dead; release every end before the
        // flags so the reap can treat the pair as fully finished.
        let _ = (upstream, downstream);
        drop(client);
        drop(server);
        up.store(true, Ordering::SeqCst);
        down.store(true, Ordering::SeqCst);
        return;
    }
    let (upstream, downstream) = (
        upstream.unwrap_or_else(|error| panic!("relay clone failed: {error}")),
        downstream.unwrap_or_else(|error| panic!("relay clone failed: {error}")),
    );
    pump(client, upstream, state.clone(), false, up);
    pump(server, downstream, state.clone(), true, down);
}

/// Whether a chunk from the TUI starts an event-stream subscription.
fn subscribes(chunk: &[u8]) -> bool {
    [&b"GET /api/event "[..], &b"GET /api/event?"[..]]
        .iter()
        .any(|request| {
            chunk
                .windows(request.len())
                .any(|window| window == *request)
        })
}

/// Copies one direction until a close or a cut, applying the relay's delay
/// to the service's direction and counting the TUI's subscriptions. A
/// detached pump ends with its sockets first, then its completion flag, so a
/// pair is never reaped while one of its pumps still holds an end.
fn pump(
    mut from: TcpStream,
    mut to: TcpStream,
    state: Arc<Mutex<Relayed>>,
    to_tui: bool,
    done: Arc<AtomicBool>,
) {
    thread::Builder::new()
        .name("busy-seat-relay-pump".into())
        .spawn(move || {
            let _guard = PumpDone(done);
            let mut chunk = [0u8; 16 * 1024];
            let mut request = trace::RequestLine::default();
            loop {
                let count = match from.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                if to_tui {
                    let delay = locked(&state).delay;
                    if !delay.is_zero() {
                        thread::sleep(delay);
                    }
                } else {
                    request.observe(&chunk[..count], &state);
                    if subscribes(&chunk[..count]) {
                        locked(&state).subscriptions += 1;
                    }
                }
                // A cut or a closed peer ends the copy; both are expected here.
                if to.write_all(&chunk[..count]).is_err() {
                    break;
                }
            }
            let _ = to.shutdown(std::net::Shutdown::Write);
            // Release both ends before the completion flag drops, so a set
            // flag means the pump has fully let go.
            drop(from);
            drop(to);
        })
        .unwrap_or_else(|error| panic!("relay pump failed to start: {error}"));
}

/// Sets the pump's completion flag on every exit path of its copy loop; the
/// pump's sockets drop before this guard, so a set flag means both ends are
/// released.
struct PumpDone(Arc<AtomicBool>);

impl Drop for PumpDone {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The reap's contract: a pair survives a reap while any pump can still hold
/// an end, and drops only once both pumps have finished and released theirs.
#[test]
fn a_still_pumping_pair_survives_a_reap() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("reap test listen failed: {error}"));
    let client = TcpStream::connect(
        listener
            .local_addr()
            .unwrap_or_else(|error| panic!("reap test local addr failed: {error}")),
    )
    .unwrap_or_else(|error| panic!("reap test connect failed: {error}"));
    let server = listener
        .accept()
        .unwrap_or_else(|error| panic!("reap test accept failed: {error}"))
        .0;
    let (up, down) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let mut relayed = Relayed {
        upstream: None,
        open: vec![OpenConnection {
            client: client
                .try_clone()
                .unwrap_or_else(|error| panic!("reap test client clone failed: {error}")),
            server: server
                .try_clone()
                .unwrap_or_else(|error| panic!("reap test server clone failed: {error}")),
            up: up.clone(),
            down: down.clone(),
        }],
        delay: Duration::ZERO,
        subscriptions: 0,
        trace: None,
        trace_changed: Arc::new(Condvar::new()),
    };
    relayed.retain_live();
    assert_eq!(
        relayed.open.len(),
        1,
        "a pair whose pumps are running must survive a reap"
    );
    up.store(true, Ordering::SeqCst);
    relayed.retain_live();
    assert_eq!(
        relayed.open.len(),
        1,
        "a pair with one live pump must survive a reap"
    );
    down.store(true, Ordering::SeqCst);
    relayed.retain_live();
    assert_eq!(
        relayed.open.len(),
        0,
        "a pair whose pumps have both released their ends is reaped"
    );
}

//! One bounded input owner exposes parser state before startup can submit.
pub(crate) mod batch;
use crossterm::event::Event;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::SyncSender,
    Arc,
};

#[derive(Default)]
pub struct State {
    quiet: AtomicBool,
    armed: AtomicBool,
    probe: AtomicBool,
    generation: AtomicU64,
}

impl State {
    pub fn observing(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }

    pub fn activity(&self, wake: &SyncSender<()>) {
        if self.observing() {
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.quiet.store(false, Ordering::Release);
            let _ = wake.try_send(());
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn requested(&self) -> bool {
        self.probe.load(Ordering::Acquire)
    }

    pub fn acknowledge(&self, idle: bool, generation: u64, wake: &SyncSender<()>) {
        if idle && self.generation() == generation && self.probe.swap(false, Ordering::AcqRel) {
            self.quiet.store(true, Ordering::Release);
            let _ = wake.try_send(());
        }
    }
}

pub struct Input {
    events: batch::Events,
    held: Option<Result<Event, String>>,
    wake: SyncSender<()>,
    state: Arc<State>,
    #[cfg(unix)]
    commands: tokio::sync::mpsc::Sender<()>,
}

impl Input {
    pub fn try_recv(&mut self) -> Result<Result<Event, String>, std::sync::mpsc::TryRecvError> {
        self.held.take().map_or_else(|| self.events.try_recv(), Ok)
    }

    /// Check the physical generation around the decoded queue too: a quiet
    /// acknowledgement cannot overtake input published after the UI's first drain.
    pub fn drained(&mut self) -> bool {
        let generation = self.state.generation();
        if !self.quiet() || self.held.is_some() {
            return false;
        }
        match self.events.try_recv() {
            Ok(event) => {
                self.held = Some(event);
                let _ = self.wake.try_send(());
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.quiet() && self.state.generation() == generation
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        }
    }

    pub fn quiet(&self) -> bool {
        self.state.quiet.load(Ordering::Acquire)
    }

    pub fn probe(&mut self) -> Result<(), String> {
        if self.quiet() || self.state.requested() {
            return Ok(());
        }
        self.state.probe.store(true, Ordering::Release);
        #[cfg(unix)]
        self.commands
            .try_send(())
            .map_err(|_| "Startup input owner stopped".to_owned())?;
        Ok(())
    }

    pub fn retire(&self) {
        if self.state.armed.swap(false, Ordering::AcqRel) {
            #[cfg(unix)]
            let _ = self.commands.try_send(());
        }
    }
}

#[cfg(unix)]
pub fn start(wake: SyncSender<()>, armed: bool) -> Result<Input, String> {
    let (tx, events) = std::sync::mpsc::sync_channel(1);
    let (commands, requests) = tokio::sync::mpsc::channel(1);
    let state = Arc::new(State::default());
    state.armed.store(armed, Ordering::Release);
    let owner = state.clone();
    let ui_wake = wake.clone();
    std::thread::Builder::new()
        .name("tui-r-input".into())
        .spawn(move || {
            let result = (|| -> Result<(), String> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .map_err(|e| e.to_string())?;
                runtime.block_on(read(&owner, requests, &wake, &tx))
            })();
            if let Err(error) = result {
                let _ = tx.send(vec![Err(error)]);
                let _ = wake.try_send(());
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Input {
        events: batch::Events::new(events),
        held: None,
        wake: ui_wake,
        state,
        commands,
    })
}

#[cfg(unix)]
async fn timer(wait: Option<std::time::Duration>) {
    match wait {
        Some(wait) => tokio::time::sleep(wait).await,
        None => std::future::pending().await,
    }
}

#[cfg(unix)]
async fn read(
    state: &State,
    mut requests: tokio::sync::mpsc::Receiver<()>,
    wake: &SyncSender<()>,
    output: &SyncSender<batch::Packet>,
) -> Result<(), String> {
    use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt, time::Duration};
    // Native O_NONBLOCK on the supported Linux/Android and BSD/macOS adapters.
    // A separate open leaves the launcher's stdin flags unchanged.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const NONBLOCK: i32 = 0x800;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const NONBLOCK: i32 = 0x4;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(NONBLOCK)
        .open("/dev/tty")
        .map_err(|e| e.to_string())?;
    // Tokio's replacement registration API is unsafe. This safe constructor
    // owns a private File: its descriptor stays open and unchanged, is never
    // duplicated, and is only borrowed below for read, satisfying registration's
    // I/O-safety requirements without exposing an arbitrary AsRawFd wrapper.
    #[expect(
        deprecated,
        reason = "private owned File satisfies AsyncFd registration's I/O-safety requirements"
    )]
    let source = tokio::io::unix::AsyncFd::new(file).map_err(|e| e.to_string())?;
    let mut resize = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        .map_err(|e| e.to_string())?;
    let mut parser = crate::input_bytes::Parser::new();
    let started = tokio::time::Instant::now();
    let mut bytes = [0; 4096];
    loop {
        let quiet_wait = (state.observing() && state.requested() && parser.idle())
            .then_some(Duration::from_millis(1));
        let wait = [quiet_wait, parser.wait(started.elapsed())]
            .into_iter()
            .flatten()
            .min();
        let events = tokio::select! {
            biased;
            ready = source.readable() => {
                let mut ready = ready.map_err(|e| e.to_string())?;
                let count = match ready.try_io(|inner| {
                    let mut file = inner.get_ref();
                    file.read(&mut bytes)
                }) {
                    Ok(result) => result.map_err(|e| e.to_string())?,
                    Err(_) => continue,
                };
                if count == 0 { return Err("Terminal input closed".into()); }
                state.activity(wake);
                parser.feed(&bytes[..count], started.elapsed())
            }
            resized = resize.recv() => {
                if resized.is_none() { return Err("Terminal resize source closed".into()); }
                let (width, height) = crossterm::terminal::size().map_err(|e| e.to_string())?;
                vec![Ok(Event::Resize(width, height))]
            }
            request = requests.recv(), if state.observing() => {
                if request.is_none() { return Err("Startup input control closed".into()); }
                continue;
            }
            _ = timer(wait) => {
                let events = parser.expire(started.elapsed());
                if events.is_empty() { state.acknowledge(parser.idle(), state.generation(), wake); }
                events
            }
        };
        let mut batch = batch::Batch::new(output, wake);
        for event in events {
            if !batch.push(event) {
                return Ok(());
            }
        }
        if !batch.flush() {
            return Ok(());
        }
    }
}

#[cfg(windows)]
pub fn start(wake: SyncSender<()>, armed: bool) -> Result<Input, String> {
    let state = Arc::new(State::default());
    state.armed.store(armed, Ordering::Release);
    let events = crate::windows_input::start(wake.clone(), state.clone())?;
    Ok(Input {
        events: batch::Events::new(events),
        held: None,
        wake,
        state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_pending_parse_cannot_acknowledge_read_retry_or_a_quiet_probe() {
        let state = State::default();
        state.armed.store(true, Ordering::Release);
        state.probe.store(true, Ordering::Release);
        let (wake, _notification) = std::sync::mpsc::sync_channel(1);
        state.activity(&wake);
        state.acknowledge(false, state.generation(), &wake);
        assert!(!state.quiet.load(Ordering::Acquire));
        assert!(state.requested());
        state.acknowledge(true, state.generation(), &wake);
        assert!(state.quiet.load(Ordering::Acquire));
        assert!(!state.requested());
        state.activity(&wake);
        assert!(!state.quiet.load(Ordering::Acquire));
    }

    #[test]
    #[cfg(unix)]
    fn a_published_event_cannot_be_overtaken_by_the_quiet_ack(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let state = Arc::new(State::default());
        state.armed.store(true, Ordering::Release);
        let (sender, events) = std::sync::mpsc::sync_channel(1);
        let (wake, _notification) = std::sync::mpsc::sync_channel(1);
        let (commands, _requests) = tokio::sync::mpsc::channel(1);
        let mut input = Input {
            events: batch::Events::new(events),
            held: None,
            wake: wake.clone(),
            state: state.clone(),
            commands,
        };
        let earlier = state.generation();
        state.activity(&wake);
        state.probe.store(true, Ordering::Release);
        state.acknowledge(true, earlier, &wake);
        assert!(
            !input.quiet(),
            "an older parser pass cannot acknowledge new input"
        );
        let event = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        sender.send(vec![Ok(event.clone())])?;
        state.acknowledge(true, state.generation(), &wake);
        assert!(!input.drained());
        assert_eq!(input.try_recv()??, event);
        assert!(input.drained());
        Ok(())
    }
}

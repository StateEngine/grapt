//! Named, cancellable phase waits and owned workers for terminal witnesses.
use super::*;

const POLL: Duration = Duration::from_millis(10);

pub(super) struct Phase {
    open: AtomicBool,
    allowance: Duration,
}

impl Phase {
    pub(super) fn new() -> Self {
        Self::within(Duration::from_secs(60))
    }
    pub(super) fn within(allowance: Duration) -> Self {
        Self {
            open: AtomicBool::new(false),
            allowance,
        }
    }
    pub(super) fn release(&self) {
        self.open.store(true, Ordering::SeqCst);
    }
    pub(super) fn wait(&self, name: &str, stop: &AtomicBool) -> Result<(), String> {
        let wait = PollDeadline::within(self.allowance);
        while wait.pending() {
            if stop.load(Ordering::SeqCst) {
                return Err(format!("SSE phase {name} cancelled"));
            }
            if self.open.load(Ordering::SeqCst) {
                return Ok(());
            }
            thread::sleep(POLL);
        }
        Err(format!(
            "SSE phase {name} exceeded its {}ms deadline",
            self.allowance.as_millis()
        ))
    }
}

pub(super) struct Worker {
    name: &'static str,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<Result<(), String>>>,
}

impl Worker {
    pub(super) fn spawn(
        name: &'static str,
        stop: Arc<AtomicBool>,
        work: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Self {
        Self {
            name,
            stop,
            handle: Some(thread::spawn(work)),
        }
    }
    pub(super) fn finish(&mut self) -> Result<(), String> {
        let wait = PollDeadline::new();
        while wait.pending() {
            if self
                .handle
                .as_ref()
                .is_none_or(|handle| handle.is_finished())
            {
                return self.handle.take().map_or(Ok(()), |handle| {
                    handle
                        .join()
                        .map_err(|_| format!("SSE worker {} panicked", self.name))?
                });
            }
            thread::sleep(POLL);
        }
        self.stop.store(true, Ordering::SeqCst);
        Err(format!(
            "SSE worker {} exceeded its join deadline: {wait}",
            self.name
        ))
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Err(error) = self.finish() {
            eprintln!("{error}");
        }
    }
}

pub(super) struct RootCleanup(pub(super) PathBuf);
impl Drop for RootCleanup {
    fn drop(&mut self) {
        process_fixture::cleanup(&self.0);
    }
}

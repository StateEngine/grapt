//! Scoped observations of native spawn failures without changing I/O errors.
//!
//! SE Code's diagnostic-only addition. Native calls and their error values
//! retain upstream behavior; no arguments, environment or handles are stored.
use std::cell::Cell;
use std::io;
use std::marker::PhantomData;
use std::rc::Rc;

/// Native inputs already frozen when `CreateProcessW` was called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launch {
    /// Creation flags requested by the caller.
    pub requested_creation_flags: u32,
    /// Flags passed to `CreateProcessW`, once that call is reached.
    pub effective_creation_flags: Option<u32>,
    /// Whether the transaction requests an initially suspended child.
    pub suspended: bool,
    /// Job handles in the frozen process attribute list.
    pub jobs: usize,
    /// Handles in the explicit inheritance list.
    pub inherited_handles: usize,
    /// Whether the transaction carries a pseudoconsole attribute.
    pub pseudoconsole: bool,
}

/// The first native failure within one synchronous observation scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    /// Name of the native call whose error was propagated.
    pub call: &'static str,
    /// Original OS error code captured before cleanup.
    pub win32_error: i32,
    /// Frozen launch inputs, if preparation reached the attribute list.
    pub launch: Option<Launch>,
}

#[derive(Debug, Default, Clone, Copy)]
struct State {
    failure: Option<Failure>,
    launch: Option<Launch>,
}

thread_local! {
    static STATE: Cell<State> = const { Cell::new(State { failure: None, launch: None }) };
}

/// One bounded, thread-local observation. Cleanup cannot replace its failure.
/// The guard cannot cross threads or an asynchronous suspension point.
#[derive(Debug)]
#[must_use = "keep the observation scope alive throughout the launch"]
pub struct Capture {
    previous: State,
    _thread: PhantomData<Rc<()>>,
}

impl Capture {
    /// Begin an empty scope, retaining the previous scope until drop.
    pub fn new() -> Self {
        Self {
            previous: STATE.with(|state| state.replace(State::default())),
            _thread: PhantomData,
        }
    }

    /// Read the first native error observed in this scope.
    pub fn failure(&self) -> Option<Failure> {
        STATE.with(|state| state.get().failure)
    }
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        STATE.with(|state| state.set(self.previous));
    }
}

/// Record a propagated OS failure, including calls made by the ConPTY owner.
pub fn record(call: &'static str, error: &io::Error) {
    let Some(win32_error) = error.raw_os_error() else {
        return;
    };
    STATE.with(|state| {
        let mut held = state.get();
        if held.failure.is_none() {
            held.failure = Some(Failure {
                call,
                win32_error,
                launch: held.launch,
            });
            state.set(held);
        }
    });
}

/// Attach an observation while returning the original result unchanged.
pub fn record_result<T>(call: &'static str, result: io::Result<T>) -> io::Result<T> {
    if let Err(error) = &result {
        record(call, error);
    }
    result
}

#[cfg(windows)]
pub(crate) fn launch(value: Launch) {
    STATE.with(|state| {
        let mut held = state.get();
        held.launch = Some(value);
        state.set(held);
    });
}

#[cfg(windows)]
pub(crate) fn effective_creation_flags(value: u32) {
    STATE.with(|state| {
        let mut held = state.get();
        if let Some(launch) = &mut held.launch {
            launch.effective_creation_flags = Some(value);
            state.set(held);
        }
    });
}

#[cfg(windows)]
pub(crate) fn last_os_error(call: &'static str) -> io::Error {
    let error = io::Error::last_os_error();
    record(call, &error);
    error
}

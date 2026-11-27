//! The console's own event source, one implementation per platform.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::Listener;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::Listener;

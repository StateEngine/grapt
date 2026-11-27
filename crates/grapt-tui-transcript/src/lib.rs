//! The conversation's rendering, below the TUI's event loop: the transcript
//! rows and their bounds, markdown and syntax highlighting, the palette, the
//! turn footer, the prompt rule and the render profiler. Nothing here reaches
//! the backend, the bridge or input handling; the TUI depends on this crate,
//! never the reverse.

pub mod highlight;
pub mod markdown;
pub mod palette;
pub mod profile;
pub mod prompt;
pub mod transcript;
pub mod turn_footer;

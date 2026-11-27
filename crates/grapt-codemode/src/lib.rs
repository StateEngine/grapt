//! Fresh JavaScript guests and a bounded private worker protocol.
//!
//! The guest can request catalogued tools. Only the parent may authorize or
//! execute those requests; this crate owns no database, provider or dispatcher.

mod bridge;
mod engine;
mod protocol;
pub use engine::run_worker;
pub use protocol::*;

pub const SOURCE_BYTES: usize = 64 * 1024;
pub const WORKER_ARGUMENT: &str = "__codemode-worker-v1";
pub const OUTPUT_BYTES: usize = 64 * 1024;
// A full catalog plus a 64 KiB source whose JSON representation expands sixfold.
pub const FRAME_BYTES: usize = 2 * 1024 * 1024;
// Catalog transport capacity must not widen outstanding host arguments.
pub(crate) const QUEUED_ARGUMENT_BYTES: usize = 1024 * 1024;
pub const ARGUMENT_BYTES: usize = 256 * 1024;
// Carry the publisher's 512 KiB aggregate with names, fallback descriptions and
// JSON escaping of up to 256 descriptions of 512 bytes each. This transport
// allowance does not change the guest's heap, stack, call or output bounds.
pub const CATALOG_BYTES: usize = 1536 * 1024;
pub const CATALOG_TOOLS: usize = 256;
pub const HOST_CALLS: usize = 64;
pub const IN_FLIGHT: usize = 8;
pub const HEAP_BYTES: usize = 16 * 1024 * 1024;
pub const STACK_BYTES: usize = 256 * 1024;
pub const INTERRUPT_VISITS: usize = 1000;
pub const JOBS: usize = 1024;
pub const DETAIL_BYTES: usize = 2 * 1024;

/// Failure text as the model may see it: control characters other than a
/// newline become spaces and the text is cut at a character boundary.
pub fn bounded_detail(text: &str) -> Option<String> {
    let mut detail = String::with_capacity(text.len().min(DETAIL_BYTES));
    for character in text.chars() {
        let character = if character.is_control() && character != '\n' {
            ' '
        } else {
            character
        };
        if detail.len().saturating_add(character.len_utf8()) > DETAIL_BYTES {
            break;
        }
        detail.push(character);
    }
    let detail = detail.trim();
    (!detail.is_empty()).then(|| detail.to_owned())
}

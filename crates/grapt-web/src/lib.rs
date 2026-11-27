//! Public-web text fetching. Callers own permission admission and artifacts;
//! this adapter owns URL validation, connection pinning, and response bounds.

mod fetch;
mod policy;
mod render;

pub use fetch::{fetch, FetchOutcome, Page};
pub use policy::{is_public_ip, validate_public_addresses, validate_public_host, WebUrl};

pub const MAX_BODY_BYTES: usize = 512 * 1024;
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// Error messages exclude URLs, response bodies, and resolver diagnostics.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FetchError {
    #[error("web URL must be HTTP(S), have a public host, contain no credentials, and fit in 8192 bytes")]
    Url,
    #[error("web fetch refused a non-public network address")]
    Address,
    #[error("web DNS lookup failed, returned no addresses, or exceeded 32 addresses")]
    Dns,
    #[error("web fetch requires a positive deadline of at most 60 seconds")]
    Budget,
    #[error("web fetch exceeded its deadline")]
    TimedOut,
    #[error("web TLS/client initialization failed")]
    Client,
    #[error("web HTTP exchange failed")]
    Transport,
    #[error("web fetch HTTP status {0}")]
    Status(u16),
    #[error("web response headers are oversized or malformed")]
    Headers,
    #[error("web fetch exceeded 10 redirects or received an invalid redirect")]
    Redirect,
    #[error("web body exceeds 512 KiB or rendered text exceeds 1 MiB")]
    Oversized,
    #[error("web fetch supports UTF-8 text, Markdown, JSON, and HTML with identity encoding")]
    ContentType,
    #[error("web response is not valid UTF-8")]
    Encoding,
    #[error("web HTML exceeds the 128-level or 20000-node rendering limit")]
    HtmlComplexity,
    #[error("web HTML conversion failed")]
    Html,
}

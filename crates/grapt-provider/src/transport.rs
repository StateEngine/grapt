//! The provider-neutral transport core shared by every route adapter.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`. The
//! contract seals what an adapter alone may do — "The selected adapter alone
//! may serialize its request into HTTP bytes. It also owns request validation,
//! auth/header placement, stream decoding, raw finish mapping, usage
//! accounting, and provider-specific retry classification."
//!
//! Nothing here breaches that seal. This module owns the parts that are
//! **transport** rather than **protocol**: SSE framing, the bounded UTF-8
//! carry, block-ordinal allocation, and the HTTP-status/`Retry-After` mapping
//! into the one closed failure vocabulary. No vendor event name, no vendor JSON
//! key and no request body appears in this module or its children.
//!
//! # Why a shared core and not four copies
//!
//! The shared transport serves three routes beside Codex. Each of the two decoder
//! defects already recorded in this crate's history (a code point split across
//! a chunk boundary; a CRLF-framed event) is a transport defect that every
//! route would have to rediscover on its own bytes. One decoder means one place
//! for that law and one place to witness it.

pub(crate) mod adapter;
pub(crate) mod blocks;
#[cfg(test)]
mod boundaries;
pub mod capture;
pub(crate) mod classify;
pub(crate) mod client;
mod client_pool;
pub(crate) mod dispatch;
pub(crate) mod error_text;
#[cfg(test)]
pub(crate) mod fixtures;
mod renewal;
mod secret_shapes;
#[cfg(any(test, feature = "loopback-testkit"))]
pub(crate) mod send_seam;
pub(crate) mod sink;
pub(crate) mod slot;
pub(crate) mod sse;
pub(crate) mod stream;
pub(crate) mod terminal_detail;

pub(crate) mod cloudflare;
mod eventstream;
mod framing;

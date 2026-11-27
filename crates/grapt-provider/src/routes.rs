//! One module per protocol family: what its bytes mean, and what its request
//! body looks like.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Provider request boundary" — "The selected adapter alone may serialize its
//! request into HTTP bytes. It also owns request validation, auth/header
//! placement, stream decoding, raw finish mapping, usage accounting, and
//! provider-specific retry classification."
//!
//! Each child owns exactly one route family's vendor vocabulary. Nothing here
//! opens a socket, allocates an ordinal, frames an event or classifies a status
//! code: those are [`crate::transport`], shared, because they are transport
//! rather than protocol.
//!
//! # Bodies are built key by key
//!
//! No `Serialize` derive, no `HashMap<String, Value>` from a caller, no
//! overlay. The oracle's transport carries an `http.body` overlay guarded by a
//! 40-name denylist (the oracle's `http.ts`) — the generic
//! bag the contract forbids — and it is deliberately not reproduced. A field
//! that a route does not support is not stripped here; it is unnameable in that
//! route's request type, and these builders are the second half of that law:
//! a type cannot stop a serializer inserting a key by string, so the emitted
//! key sets are asserted directly.

pub(crate) mod anthropic;
mod anthropic_input;
mod anthropic_search;
mod anthropic_tools;
pub(crate) mod body;
pub(crate) mod compat;
mod compatible_input;
mod image_content;
pub(crate) mod request_fingerprint;
pub(crate) mod request_validation;
mod response_groups;
pub(crate) mod responses;
mod responses_terminal;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod response_groups_tests;

pub(crate) mod bedrock;

//! The platform vertical, split by battery under the workspace god-file
//! ceiling.
//!
//! The shared fixtures live in `shared.rs` and each battery includes them,
//! because these are integration tests: they may not reach into the crate, so
//! a helper cannot be a `pub(crate)` item somewhere central.

#[path = "platform_vertical/shared.rs"]
mod shared;

#[path = "platform_vertical/publication.rs"]
mod publication;

#[path = "platform_vertical/accounts.rs"]
mod accounts;

#[path = "platform_vertical/listing_metadata.rs"]
mod listing_metadata;

#[path = "platform_vertical/api_key_primary.rs"]
mod api_key_primary;

#[path = "platform_vertical/connections.rs"]
mod connections;

#[path = "platform_vertical/store_ready.rs"]
mod store_ready;

#[path = "platform_vertical/aws_owner.rs"]
mod aws_owner;

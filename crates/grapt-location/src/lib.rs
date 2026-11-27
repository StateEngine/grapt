//! Location-owned bind intent and listener acquisition.
//!
//! This crate owns exactly one boundary: the step between a validated,
//! bounded statement of *where to listen* and an acquired operating-system
//! listener held by exactly one owner.
//!
//! # What it owns
//!
//! * [`BindHost`], [`BindPort`] and [`ListenerBindPlan`] — a closed bind intent
//!   in which "no port", "port zero" and "search from a base port" cannot
//!   collapse into one nullable integer.
//! * [`BindLimits`] — positive, `Default`-free bounds on resolution and on
//!   bind attempts.
//! * [`NameResolver`] — injected, bounded name resolution, with the
//!   literal-address-only production adapter [`LiteralAddressResolver`] and the
//!   [`DeterministicNameResolver`] fake.
//! * [`ListenerBinder`], [`BoundListener`], [`BindReceipt`] and
//!   [`ClosedListener`] — acquisition, single non-clone ownership, a truthful
//!   requested-versus-observed receipt, and an explicit close distinct from the
//!   `Drop` backstop.
//! * [`BindError`] and [`ResolveError`] — failures classified from
//!   [`std::io::ErrorKind`], never from operating-system error text.
//!
//! # What it deliberately does not own
//!
//! No HTTP, routes, headers, bodies, authentication, authorization, DTOs, SSE
//! or WebSockets. No process registration, deployment, CLI defaults, service
//! switching or health endpoint. No store, mail, provider transport, session
//! runtime, tools, child processes or config watching. No ambient DNS lookup
//! and no ambient default bind plan.
//!
//! Those exclusions are the point of the crate existing separately: a socket is
//! a location-scoped resource with its own failure surface, while HTTP policy,
//! principals and response semantics belong to the API layer above it. A bound
//! listener is not a ready server, and nothing here claims otherwise.
//!
//! # Dependency direction
//!
//! `grapt-types <- grapt-location <- grapt-api`. This crate depends on no
//! other workspace crate. It does not yet consume a `LocationId`, and an
//! unused edge added merely to resemble the eventual graph would be a false
//! statement about what this foundation needs; the edge arrives when a receipt
//! actually becomes location-bound. `tests/dependency_direction.rs` enforces
//! that mechanically rather than by convention.

mod config;
mod listener;

pub use config::{
    AdapterUnavailableReason, BindHost, BindHostName, BindLimits, BindPort, BoxFuture,
    DeterministicNameResolver, HostNameError, LimitError, LimitField, ListenerBindPlan,
    LiteralAddressResolver, MaxBindAttempts, MaxResolvedAddresses, NameResolver, ResolveError,
    ResolvedAddresses, MAX_HOST_NAME_BYTES,
};
pub use listener::{
    AcceptError, AcceptedConnection, BindError, BindReceipt, BoundListener, ClosedListener,
    ListenerBinder,
};

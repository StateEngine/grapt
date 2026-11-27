//! Form blocker domain. The blocker machine lives in `grapt-types`; this
//! crate owns the form event vocabulary, strict decode, and (next slice) the
//! write-side service.
//!
//! CONTRAST WITH PERMISSION (contract §"Permission and Form blocker
//! lifecycle"): form suspension is NON-TERMINAL. A form may be answered or
//! cancelled AFTER suspension or a service replacement — there is no
//! `NotLive` refusal for a direct reply to a suspended form. That asymmetry
//! is the whole reason Permission and Form are separate services over shared
//! laws rather than one generic blocker.

pub mod answer;
pub mod catalog;
pub mod date;
pub mod elicitation;
pub mod event;
pub mod global;
pub mod global_catalog;
pub mod service;

pub use event::{FormEvent, FormEventError};
pub use service::{FormError, FormService};

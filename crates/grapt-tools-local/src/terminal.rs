//! Portable terminal output, input normalization, stop control and shim contracts.
//!
//! The application owns process adapters, authorization and durable observations.

pub mod output;
pub mod shim_protocol;
pub mod stop;
pub mod windows_input;

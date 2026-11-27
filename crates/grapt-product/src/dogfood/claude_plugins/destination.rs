//! Exclusive, no-follow publication adapted from the existing CLI importer.
//! Copyright 2025 OpenAI.
//! Apache-2.0. Modified for the product installer and existing nix dependency.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(super) use unix::Parent;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::Parent;

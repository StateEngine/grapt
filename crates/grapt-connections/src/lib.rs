//! A profile's provider connections.
//!
//! The approved connections design (dated 2026-11-02, kept on the box under
//! /srv/grapt/diagnostics) moves a profile from one provider block to many connections. This first slice (C0)
//! replaces an existing profile's ChatGPT sign-in in place on today's
//! single-provider layout; the connection store generalizes it.

pub mod catalog;
pub mod chatgpt;
pub mod store;

pub mod status;

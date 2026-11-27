//! The verified tool-binary activation boundary (security-continuity row 1).
//!
//! Law consumed, by name: section 8 row 1 of
//! `SECODE_RUST_RECONSTRUCTION_DISPOSITION_2026-08-07.md` — a tool binary
//! activates only when its archive sha256, source URL and per-target pin all
//! match; a revoked or re-cut pin at the same version refuses; identity write
//! failure rolls back activation. The disposition names this crate as the
//! Rust owner. The complete typed contract, bounds and refusal taxonomy are
//! admitted in `docs/toolchain-activation/CONTRACT.md` and
//! `fixtures/toolchain-activation/contract.json` (TOOLCHAIN-ACTIVATION-01).
//!
//! ## The law this crate exists to hold
//!
//! Presence of a binary on disk is NOT evidence of provenance. A binary is
//! attributable only when the identity record beside it decodes strictly AND
//! matches the CURRENT pin — tool, version, platform, arch, target, source
//! and archive digest all equal — AND the bytes on disk still hash to the
//! record's binary digest. Version and target alone never match: revoking a
//! compromised digest or upstream re-cutting a release changes the pin while
//! leaving version and target identical, and a record written under the old
//! pin must not keep the old binary alive.
//!
//! ## Four boundaries this crate does not cross
//!
//! * No network. The core logic is network-free by construction; the only
//!   shipped source adapter ([`FileSource`]) reads a local file. A network
//!   source is an explicit release-input boundary, refused
//!   [`RefusalReason::SourceUnsupported`], never a silent capability.
//! * No ambient clock or randomness. The identity's `installed_at_ms` comes
//!   from an injected [`grapt_types::ClockAuthority`]; the deterministic
//!   tests inject a fixed clock.
//! * No second archive stack. The only supported format is uncompressed
//!   ustar tar, decoded under declared bounds by [`archive`]; a pin naming
//!   anything else is a malformed pin, not a format to discover.
//! * No product behavior. This crate owns the activation boundary; it does
//!   not decide WHEN a product activates a tool, runs the binary, or
//!   installs anything into a real user directory.
//!
//! ## The ordering that makes a crash recoverable
//!
//! Staging is untrusted and disposable: the archive is decoded in memory
//! and its unpacked tree lives only under `tools/<tool>/.staging/`,
//! removed however the attempt ends, including a crashed one (the next
//! attempt clears it first). Publishing is ONE rename: the previous
//! canonical executable, when there is one, is first hard-linked to
//! `.last-known-good` and the re-verified staged copy is then renamed onto
//! the canonical path, replacing it in place. A crash in any window therefore
//! leaves either the previous executable or the new one at the canonical
//! path, never neither, and the identity record is written LAST: if it cannot
//! be written, the activation rolls back rather than leave an unattributable
//! executable at the canonical path.
//!
//! ## Diagnostics
//!
//! Every refusal that has a decoded pin entry names the failed tool,
//! version, target, source and pin archive digest; earlier refusals name
//! the facts they have. No refusal message carries a filesystem path or a
//! secret: the layout is fixed by [`LAYOUT`], so a path in a diagnostic
//! adds no information and leaks the operator's tree.

mod activate;
mod archive;
mod identity;
mod pin;
mod refusal;
mod source;
mod value;

pub use activate::{activate, ActivationReceipt, ActivationRequest, RefusalContext, LAYOUT};
pub use archive::{decode_tar, DecodedArchive};
pub use identity::InstalledIdentity;
pub use pin::{attribute, Pin, PinAttribution, PinTable};
pub use refusal::{Refusal, RefusalReason};
pub use source::{ArchiveSource, FileSource};
pub use value::{ArchiveFormat, EntryName, Sha256, SourceUrl, TargetTriple, ToolName, ToolVersion};

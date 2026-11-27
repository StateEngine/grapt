//! Strict, bounded decoding for user-authored configuration documents.
//!
//! Law: master `docs/reports/SECODE_RUST_RECONSTRUCTION_DISPOSITION_2026-08-07.md`
//! Config row — "PRESERVE-CONTRACT + REDESIGN" — and the constitution's rule that
//! "User-authored configuration and public request decoding preserve structured
//! paths through a `serde_path_to_error`-class boundary."
//!
//! # What this crate owns TODAY
//!
//! [`bounds`] — named bound types with no `Default` and no free constructor.
//! [`document`] — the decoded shape: ordered objects, exact number literals,
//! `Null` as a value, byte spans, structured value paths. [`jsonc`] — the
//! decoder: JSON plus the comment and trailing-comma forms the oracle accepts.
//!
//! [`source_bounds`] — the bounds a discovery runs under, and the one check
//! that keeps a rank space large enough to number its slots injectively.
//! [`source`] — source custody: which slots exist, in what order, and what was
//! found at each.
//!
//! # What is absent, and why it is absent rather than missing
//!
//! The IA0-2 configuration lane exceeded L2's insertion ceiling, so the lane
//! was split. **U1a** is the decoder ([`bounds`], [`document`], [`jsonc`]).
//! **U1b1** is source custody ([`source_bounds`], [`source`]): discovery order,
//! slot-derived ranks, canonical path identity and bounded capture.
//!
//! **U1b2a** owns what sits on top of that census, and is PRESENT: [`ledger`]
//! (the document ledger, the invalid-contributor state, the retained source
//! census and the completeness boundary), [`mod@resolve`] (ordered contribution
//! and provenance), and [`layer`] (the bridge that mints
//! `grapt_policy::ConfigLayer`).
//!
//! With U1b2a landed this crate can say WHICH documents contribute, in what
//! order, from where, and whether the configuration is complete — which is what
//! the effective-authority graph needs from it.
//!
//! **U1b2b** is holding a complete configuration ACROSS loads, and is PRESENT:
//! [`mod@reload`] (the configuration session, its monotone generation, and the
//! rule that a generation advances exactly once per complete load while a
//! refused reload changes nothing).
//!
//! With U1b2b landed a caller no longer gets one complete answer per `admit`
//! and is left to decide for itself what changed: it holds a session, asks it
//! to look again, and compares one number. What a session still does NOT do is
//! watch the filesystem or DIFF two configurations — both are documented, with
//! their reasons, in [`mod@reload`].
//!
//! # What this crate does NOT decide
//!
//! It does not MERGE. Folding the ordered contributions into one effective
//! value per setting is a domain question — whether two objects combine or the
//! nearer replaces, whether an array appends or replaces, whether a `null`
//! clears an inherited key — and merging is outside U1b's contract. Those
//! answers belong to the later policy/domain fold, which knows what each
//! setting means. This crate reports the ordered contributors and their
//! provenance and stops there, deliberately.
//!
//! `GRAPT_CONFIG_CONTENT` and the well-known integration configs are explicit
//! U1 debt owned by a later completion slice: both need the
//! variable-substitution seam, and the first ranks ABOVE every other source.
//!
//! # No panics
//!
//! There is no panicking path outside `#[cfg(test)]`. Every refusal is a typed
//! error: [`bounds::BoundRefusal`] and [`jsonc::JsoncError`]. Bounds, their
//! order, and what the byte cap does NOT claim are documented in [`jsonc`].

pub mod bounds;
pub mod document;
pub mod jsonc;
pub mod layer;
pub mod ledger;
pub mod reload;
pub mod resolve;
pub mod source;
pub mod source_bounds;

pub use bounds::{
    BoundRefusal, ConfigBounds, MaxAncestorDepth, MaxDocumentBytes, MaxDocumentNodes, MaxDocuments,
    MaxValueDepth,
};
pub use document::{
    ByteSpan, ConfigKey, ConfigNumber, ConfigValue, PathSegment, Redacted, Spanned, ValuePath,
};
pub use jsonc::{decode, JsoncError, JsoncErrorKind};
pub use layer::mint_layers;
pub use ledger::{
    admit, CompleteLedger, ConfigLedger, ContributorState, IncompleteLedger, InvalidContributor,
    LedgerEntry, LedgerRefusal,
};
pub use reload::{ConfigSession, Generation, RefusedReload, ReloadRefusal};
pub use resolve::{resolve, Contribution, Provenance, Resolution};
pub use source::{
    discover, ConfigFileName, ConfigSourcePath, DeterministicSourceReader, DiscoveredSource,
    DiscoveryPlan, DiscoveryRefusal, PathRefusal, SourceCensus, SourceIoError, SourceOutcome,
    SourceProbe, SourceRank, SourceReader, SourceSlot, StdSourceReader,
};
pub use source_bounds::{MaxPathBytes, SourceBounds, SourceBoundsRefusal};

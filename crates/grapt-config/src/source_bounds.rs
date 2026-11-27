//! The bounds source custody runs under, and the one cross-bound check that
//! keeps a walk from outrunning its own document bound.
//!
//! U1a's [`crate::bounds`] owns the bounds a DECODE runs under: document bytes,
//! value depth, node count, ancestor depth, document count. Three of those are
//! discovery-time bounds and are reused here verbatim rather than restated —
//! restating a ceiling is how two modules come to disagree about it.
//!
//! What is new here is [`MaxPathBytes`], because a discovered path is itself
//! unbounded input: an ancestor walk over a hostile tree can produce a path
//! longer than any document it names, and [`crate::source::ConfigSourcePath`]
//! allocates for it.
//!
//! # Why this is a separate module from `bounds`
//!
//! `bounds` is U1a's file and this slice does not edit it. The `bound!` macro
//! that generates U1a's five types is private to that module, so `MaxPathBytes`
//! is written out by hand below. One hand-written bound is a smaller cost than
//! either reaching into another slice's file or exporting a macro across the
//! seam for a single caller. [`BoundRefusal`] IS shared, because a second
//! refusal enum saying the same two things would make "zero" and "above the
//! ceiling" mean two different types to a matching caller.
//!
//! # The cross-bound check
//!
//! Every other bound in this crate is independent of the others. These two are
//! not: the ancestor depth decides how many SLOTS exist, and every slot is a
//! place a document may be captured from. A bound set whose walk admits more
//! slots than [`MaxDocuments`] admits documents is a bound set that can exceed
//! its own document bound, so the combination is refused HERE, once, at
//! construction.
//!
//! Refusing it at construction is what lets
//! [`crate::source::discover`] carry no document counter. A runtime counter
//! would be a second place to enforce the same law and a second place to forget
//! it; a bound set that cannot be built cannot be exceeded.
//!
//! Note what this check is NOT about. Ranks are not numbered into this bound —
//! [`crate::source::SourceSlot::rank`] does not see a bound set at all. See its
//! documentation for why.

use crate::bounds::{BoundRefusal, ConfigBounds, MaxAncestorDepth, MaxDocumentBytes, MaxDocuments};

/// How many bytes one discovered path may occupy, canonicalised.
///
/// The ancestor walk is bounded by [`MaxAncestorDepth`], but depth alone does
/// not bound path LENGTH: sixty-four directories may each carry a name of any
/// size. This is the bound that makes a canonical path identity a bounded
/// allocation rather than an unbounded one wearing a bounded walk's clothes.
///
/// The declared ceiling is 4096, POSIX `PATH_MAX`. It is this crate's declared
/// maximum for the dimension and not a machine limit: Windows admits longer
/// paths under its extended form, and a config lane that accepted them would be
/// admitting an input no other platform in this workspace can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MaxPathBytes(usize);

impl MaxPathBytes {
    /// This dimension's declared ceiling.
    pub const CEILING: usize = 4096;

    pub fn try_new(value: usize) -> Result<Self, BoundRefusal> {
        if value == 0 {
            return Err(BoundRefusal::Zero {
                bound: "max path bytes",
            });
        }
        if value > Self::CEILING {
            return Err(BoundRefusal::AboveCeiling {
                bound: "max path bytes",
                value,
                ceiling: Self::CEILING,
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> usize {
        self.0
    }
}

/// Why a source bound SET was refused, as opposed to why one bound was.
///
/// Separate from [`BoundRefusal`] on purpose: that enum answers "is this number
/// a bound?", which each bound decides alone. This one answers "do these bounds
/// agree with each other?", which no single bound can see.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceBoundsRefusal {
    /// The slots this bound set admits outnumber the documents it admits.
    ///
    /// `slots` counts every slot the walk can produce: the global slot plus one
    /// per ancestor step, the start directory included.
    #[error("a walk of {slots} source slots can exceed the {documents} document bound")]
    MoreSlotsThanDocuments { slots: usize, documents: usize },
}

/// The complete bound set one source discovery runs under.
///
/// Constructed rather than defaulted, and constructed FALLIBLY, which the
/// decode-side [`ConfigBounds`] is not: this set has an agreement to check and
/// that one does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBounds {
    document_bytes: MaxDocumentBytes,
    ancestor_depth: MaxAncestorDepth,
    documents: MaxDocuments,
    path_bytes: MaxPathBytes,
}

impl SourceBounds {
    pub fn try_new(
        document_bytes: MaxDocumentBytes,
        ancestor_depth: MaxAncestorDepth,
        documents: MaxDocuments,
        path_bytes: MaxPathBytes,
    ) -> Result<Self, SourceBoundsRefusal> {
        let slots = Self::slot_count(ancestor_depth);
        if slots > documents.value() {
            return Err(SourceBoundsRefusal::MoreSlotsThanDocuments {
                slots,
                documents: documents.value(),
            });
        }
        Ok(Self {
            document_bytes,
            ancestor_depth,
            documents,
            path_bytes,
        })
    }

    /// The discovery-time subset of a decode bound set, plus the path bound.
    ///
    /// A caller that already holds [`ConfigBounds`] should not have to take it
    /// apart and risk pairing one run's ancestor depth with another's document
    /// count.
    pub fn from_config_bounds(
        bounds: ConfigBounds,
        path_bytes: MaxPathBytes,
    ) -> Result<Self, SourceBoundsRefusal> {
        Self::try_new(
            bounds.document_bytes(),
            bounds.ancestor_depth(),
            bounds.documents(),
            path_bytes,
        )
    }

    /// How many slots an ancestor depth admits: the global slot, plus the start
    /// directory, plus each step above it.
    ///
    /// `MaxAncestorDepth` counts the start directory, so the walk contributes
    /// exactly `ancestor_depth` slots and the global slot is the one more.
    /// Saturating because a ceiling of `usize::MAX` is refused by the bound
    /// itself and this must not be the place that wraps if it ever is not.
    fn slot_count(ancestor_depth: MaxAncestorDepth) -> usize {
        ancestor_depth.value().saturating_add(1)
    }

    /// How many slots a discovery under this bound set can produce.
    ///
    /// Bounded by [`MaxDocuments`], which is checked at construction, so this is
    /// also the most documents such a discovery can capture.
    pub fn slots(self) -> usize {
        Self::slot_count(self.ancestor_depth)
    }

    pub fn document_bytes(self) -> MaxDocumentBytes {
        self.document_bytes
    }

    pub fn ancestor_depth(self) -> MaxAncestorDepth {
        self.ancestor_depth
    }

    pub fn documents(self) -> MaxDocuments {
        self.documents
    }

    pub fn path_bytes(self) -> MaxPathBytes {
        self.path_bytes
    }
}

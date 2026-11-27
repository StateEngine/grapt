//! Every bound this crate enforces, as a named type with no `Default`.
//!
//! `AGENTS.md`: "Bounded everything." A bound with a `Default` is a bound
//! nobody decided, so none of these implements it and none has a free
//! constructor. `tests/probes/01_bounds_have_no_default.rs` holds that at
//! compile time, which no runtime assertion can.
//!
//! The refusal is typed rather than an `Option` because zero and the ceiling are
//! different mistakes: zero asks for a bound admitting nothing, `usize::MAX`
//! asks for no bound at all wearing a bound's type.
//!
//! No doc literal here uses a backslash line-continuation: `cargo fmt` can join
//! one onto a single line and bake its indentation into the text, and the
//! artifact appears AFTER authoring, so reading your own source never shows it.

/// Why a bound was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoundRefusal {
    /// A bound of zero admits nothing. It is never what a caller meant.
    #[error("{bound} must be greater than zero")]
    Zero { bound: &'static str },
    /// Above the ceiling the value stops being a bound. The ceiling is this
    /// crate's declared maximum for that dimension, not a machine limit.
    #[error("{bound} of {value} exceeds the declared ceiling {ceiling}")]
    AboveCeiling {
        bound: &'static str,
        value: usize,
        ceiling: usize,
    },
}

macro_rules! bound {
    ($name:ident, $label:literal, $ceiling:expr, $($doc:literal),+ $(,)?) => {
        $(#[doc = $doc])+
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(usize);

        impl $name {
            /// This dimension's declared ceiling.
            pub const CEILING: usize = $ceiling;

            pub fn try_new(value: usize) -> Result<Self, BoundRefusal> {
                if value == 0 {
                    return Err(BoundRefusal::Zero { bound: $label });
                }
                if value > Self::CEILING {
                    return Err(BoundRefusal::AboveCeiling {
                        bound: $label,
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
    };
}

bound!(
    MaxDocumentBytes,
    "max document bytes",
    4 * 1024 * 1024,
    "Hard capture bound on one configuration document, checked BEFORE the bytes",
    "are decoded to text. Master §3.3 places the hard bound at the I/O boundary,",
    "before unbounded allocation, not after a value has been materialised.",
);

bound!(
    MaxAncestorDepth,
    "max ancestor depth",
    64,
    "How many directories the ancestor walk may visit, counting the start",
    "directory. The frozen oracle's `FileSystem.up` walks to the filesystem root",
    "with no bound at all; that is the unbounded dimension this replaces.",
);

bound!(
    MaxDocuments,
    "max documents",
    256,
    "How many configuration documents one ledger may carry. It also bounds the",
    "rank space, which is what keeps rank assignment injective and therefore",
    "keeps `grapt_policy::SnapshotBuilder` free of a `PrecedenceConflict`.",
);

bound!(
    MaxValueDepth,
    "max value depth",
    64,
    "How deeply arrays and objects may nest. Checked BEFORE descending, so a",
    "hostile document refuses instead of exhausting the stack. This crate has",
    "no panicking path, and an unbounded recursive descent would have one.",
);

bound!(
    MaxDocumentNodes,
    "max document nodes",
    100_000,
    "How many values one document may contain in total. Byte length alone does",
    "not bound node count: a document of nested empty arrays is cheap in bytes",
    "and expensive in nodes.",
);

/// The complete bound set one discovery or reload runs under.
///
/// Grouped into one value on purpose. Passing five separate bounds to every
/// entry point invites a call site that supplies four and defaults the fifth,
/// and there is no default to supply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigBounds {
    document_bytes: MaxDocumentBytes,
    ancestor_depth: MaxAncestorDepth,
    documents: MaxDocuments,
    value_depth: MaxValueDepth,
    document_nodes: MaxDocumentNodes,
}

impl ConfigBounds {
    pub fn new(
        document_bytes: MaxDocumentBytes,
        ancestor_depth: MaxAncestorDepth,
        documents: MaxDocuments,
        value_depth: MaxValueDepth,
        document_nodes: MaxDocumentNodes,
    ) -> Self {
        Self {
            document_bytes,
            ancestor_depth,
            documents,
            value_depth,
            document_nodes,
        }
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

    pub fn value_depth(self) -> MaxValueDepth {
        self.value_depth
    }

    pub fn document_nodes(self) -> MaxDocumentNodes {
        self.document_nodes
    }
}

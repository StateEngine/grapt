//! Which discovered documents became contributors, and what happened to the
//! ones that did not.
//!
//! This is the first half of **U1b2** of the IA0-2 configuration lane. U1b1
//! answered *which slots exist, in what order, and what was found at each* and
//! deliberately answered nothing about meaning. The ledger is the first module
//! that reads a captured document as a document: it decodes each captured
//! slot, and records the result as either an admitted contributor or a typed
//! invalid one.
//!
//! # The three laws this module exists to hold
//!
//! **A capture that cannot be decoded is an INVALID CONTRIBUTOR, never a
//! missing one.** A document that was found, read whole, and then failed to
//! decode is a fact about the author's file, not about the filesystem. Dropping
//! it would make a syntax error indistinguishable from an absent file, which is
//! precisely how a typo silently becomes a default. Every non-admitted capture
//! keeps its slot, its rank, its path and the typed reason.
//!
//! **The ledger is bounded BEFORE it admits, by a bound derived from the
//! caller's [`ConfigBounds`].** [`MaxDocuments`] caps how many contributors a
//! single resolution may carry. The cap is applied while admitting rather than
//! after, so a census with a thousand captured slots cannot allocate a thousand
//! decoded documents on the way to being refused.
//!
//! **Only a captured slot can become a ledger row.** The census already
//! distinguishes absent, vanished, not-a-file, unreadable, over-bound and
//! duplicate. Re-recording those here would create a second population of the
//! same facts, and two populations of the same facts drift. A reader that wants
//! to know why a slot produced nothing asks the census; a reader that wants to
//! know why a *document* produced nothing asks the ledger.
//!
//! # The completeness boundary
//!
//! A ledger that carries ANY invalid contributor is INCOMPLETE, and an
//! incomplete ledger can neither resolve nor mint a layer. That is enforced
//! structurally, not by a check every caller has to remember: [`resolve`] and
//! [`mint_layers`] name [`CompleteLedger`], which has a private field and
//! exactly one constructor — [`ConfigLedger::complete`]. There is no way to
//! reach the authority graph from a [`ConfigLedger`] without passing that
//! check, and no way to build a [`CompleteLedger`] outside this module.
//!
//! The reason it is all-or-nothing is the failure it prevents. Resolving a
//! ledger whose STRONGEST contributor failed to decode would silently promote
//! the next-weaker document to authoritative, and the program would run under
//! settings the author had overridden and believed were no longer in force. A
//! syntax error in the nearest file must refuse the generation, never quietly
//! expose the file it was supposed to override.
//!
//! [`resolve`]: crate::resolve::resolve
//! [`mint_layers`]: crate::layer::mint_layers
//!
//! # What is still absent
//!
//! Ordered contribution and provenance sit on top of this ledger and live in
//! [`mod@crate::resolve`]; the `grapt_policy::ConfigLayer` bridge lives in
//! [`crate::layer`]. Neither MERGES: folding contributors into one domain value
//! is out of this crate's contract and belongs to the later policy/domain fold.

use core::fmt;

use crate::bounds::{ConfigBounds, MaxDocuments};
use crate::document::{ConfigValue, Spanned};
use crate::jsonc::{decode, JsoncError};
use crate::source::{ConfigSourcePath, SourceCensus, SourceOutcome, SourceRank, SourceSlot};

/// Why a captured document did not become an admitted contributor.
///
/// Closed and exhaustively matched, for the same reason [`SourceOutcome`] is: a
/// new way for a document to be rejected is a decision somebody makes here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidContributor {
    /// The bytes were captured whole and did not decode.
    Undecodable(JsoncError),
    /// The document decoded, and its top level is not an object.
    ///
    /// A configuration document contributes named settings. A top-level array
    /// or scalar has no keys to contribute under and no way to be addressed by
    /// a nearer slot, so admitting one would produce a contributor that
    /// ordered contribution could only ignore.
    NotAnObject {
        /// The variant label of what was found. The label only: a top-level
        /// scalar is as secret-bearing as any other captured byte.
        kind: &'static str,
    },
}

impl fmt::Display for InvalidContributor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidContributor::Undecodable(error) => write!(f, "undecodable: {error}"),
            InvalidContributor::NotAnObject { kind } => {
                write!(f, "top level is {kind}, not an object")
            }
        }
    }
}

/// What became of one captured document.
///
/// The admitted arm carries the decoded document AND the canonical bytes it
/// decoded from. The bytes are kept because the `ConfigLayer` bridge mints a
/// keyed identity over exactly the layer's canonical bytes, and re-encoding a
/// decoded document to recover them would mint an identity over a
/// re-serialization rather than over what the author wrote.
#[derive(Clone, PartialEq, Eq)]
pub enum ContributorState {
    /// Decoded, and shaped like a contributor.
    Admitted {
        document: Spanned<ConfigValue>,
        bytes: Vec<u8>,
    },
    /// Captured, and not a contributor. The reason is typed and kept.
    Invalid(InvalidContributor),
}

/// Kind and reason only, never content. A captured configuration document is
/// the ordinary home of an API key; `crate::document::Redacted` sets the
/// precedent that even a length is a weak oracle rather than harmless shape.
impl fmt::Debug for ContributorState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContributorState::Admitted { .. } => f.write_str("Admitted"),
            ContributorState::Invalid(reason) => write!(f, "Invalid({reason})"),
        }
    }
}

/// One row of the ledger: a captured slot, and what it turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    slot: SourceSlot,
    rank: SourceRank,
    path: ConfigSourcePath,
    state: ContributorState,
}

impl LedgerEntry {
    /// The slot this document came from.
    pub fn slot(&self) -> SourceSlot {
        self.slot
    }

    /// The slot-derived precedence. Larger wins, exactly as in the census.
    pub fn rank(&self) -> SourceRank {
        self.rank
    }

    /// The path the document was captured from.
    pub fn path(&self) -> &ConfigSourcePath {
        &self.path
    }

    /// What the capture turned out to be.
    pub fn state(&self) -> &ContributorState {
        &self.state
    }

    /// The decoded document, and `None` for an invalid contributor.
    pub fn document(&self) -> Option<&Spanned<ConfigValue>> {
        match &self.state {
            ContributorState::Admitted { document, .. } => Some(document),
            ContributorState::Invalid(_) => None,
        }
    }

    /// The canonical bytes this contributor decoded from, and `None` for an
    /// invalid contributor.
    pub fn bytes(&self) -> Option<&[u8]> {
        match &self.state {
            ContributorState::Admitted { bytes, .. } => Some(bytes),
            ContributorState::Invalid(_) => None,
        }
    }

    /// Why this capture is not a contributor, and `None` when it is one.
    pub fn invalid(&self) -> Option<&InvalidContributor> {
        match &self.state {
            ContributorState::Admitted { .. } => None,
            ContributorState::Invalid(reason) => Some(reason),
        }
    }
}

/// Why a ledger could not be built at all.
///
/// Distinct from a per-document [`InvalidContributor`] for the same reason
/// [`crate::source::DiscoveryRefusal`] is distinct from [`SourceOutcome`]: this
/// is a failure of the whole operation, and returning a partial ledger that
/// reads like a complete one is how a truncated configuration becomes a silent
/// one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LedgerRefusal {
    /// The census carries more captured documents than the bound admits.
    ///
    /// Reported with the bound and the count that exceeded it, never with the
    /// documents: a refusal diagnostic that quoted the contributors would be a
    /// disclosure channel for exactly the material the bound exists to cap.
    #[error("the census carries {found} captured documents; the bound admits {bound}")]
    TooManyDocuments { found: usize, bound: usize },
}

/// The documents a census produced, in ascending precedence.
///
/// Ordering is inherited from the census and is farthest-to-nearest: a reader
/// that folds the ledger front-to-back gets nearest-wins without knowing that
/// is what it is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLedger {
    census: SourceCensus,
    entries: Vec<LedgerEntry>,
}

impl ConfigLedger {
    /// The census this ledger was admitted from, kept whole.
    ///
    /// Kept rather than summarised so there is ONE population of source facts
    /// instead of two that can drift — the rule this module already states
    /// about not re-recording census outcomes as ledger rows. A holder of a
    /// configuration needs it: `Absent`, `Vanished`, `Unreadable` and
    /// `NotConfigured` are four different answers about a slot, and all four
    /// produce zero ledger rows. Comparing admitted rows alone would report a
    /// tree whose nearest file just became unreadable as unchanged.
    ///
    /// The cost is stated rather than hidden: the captured bytes appear both
    /// here and in the admitted contributors that decoded from them, so a held
    /// census is bounded above by
    /// [`MaxDocuments`] × [`crate::bounds::MaxDocumentBytes`] a second time.
    /// Both bounds were already enforced before this ledger existed.
    pub fn census(&self) -> &SourceCensus {
        &self.census
    }

    /// Every ledger row, weakest first.
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// Only the rows that became contributors, still in ascending precedence.
    pub fn admitted(&self) -> impl Iterator<Item = &LedgerEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.document().is_some())
    }

    /// Only the rows that were captured and rejected, still in ascending
    /// precedence.
    ///
    /// This is the diagnostic view of exactly what [`ConfigLedger::complete`]
    /// refuses on. A caller inspects it to report WHICH documents are
    /// malformed; it does not have to consult it to stay safe, because an
    /// incomplete ledger cannot reach resolution or the authority graph at
    /// all.
    pub fn invalid(&self) -> impl Iterator<Item = &LedgerEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.invalid().is_some())
    }

    /// Admit this ledger to the authority graph, or refuse it whole.
    ///
    /// Succeeds only when every captured document became a contributor. The
    /// refusal names the STRONGEST invalid contributor: an invalid document at
    /// a high rank is exactly the case where continuing would expose a weaker
    /// document as authoritative, so it refuses rather than resolving without
    /// it.
    ///
    /// Consumes the ledger. A caller that wants the diagnostic rows keeps its
    /// own clone, or reads them from [`CompleteLedger::ledger`] on success.
    pub fn complete(self) -> Result<CompleteLedger, IncompleteLedger> {
        // An OBSTRUCTED slot refuses the generation before any document is
        // judged. A slot that was looked at and found empty is a configuration
        // the author wrote; a slot that could not be read is a configuration
        // nobody has seen, and resolving without it would run the program under
        // the settings the unreadable file was there to override.
        if let Some(source) = self
            .census
            .entries()
            .iter()
            .filter(|entry| obstruction_of(entry.outcome()).is_some())
            .max_by_key(|entry| entry.rank().value())
        {
            // UNREACHABLE otherwise: selected by `obstruction_of(..).is_some()`.
            if let Some(kind) = obstruction_of(source.outcome()) {
                let obstructed = self
                    .census
                    .entries()
                    .iter()
                    .filter(|entry| obstruction_of(entry.outcome()).is_some())
                    .count();
                return Err(IncompleteLedger::Obstructed {
                    obstructed,
                    slots: self.census.entries().len(),
                    strongest_slot: source.slot(),
                    strongest_rank: source.rank().value(),
                    kind,
                });
            }
        }
        let total = self.entries.len();
        // Strongest LAST: the ledger ascends in precedence, so the final
        // invalid row is the highest-ranked one. `max_by_key` over the rank
        // would answer the same question today and would keep answering it if
        // the ordering ever stopped being ascending, which is why the rank is
        // consulted rather than the position.
        let strongest = self
            .entries
            .iter()
            .filter(|entry| entry.invalid().is_some())
            .max_by_key(|entry| entry.rank().value());
        match strongest {
            None => Ok(CompleteLedger { ledger: self }),
            Some(entry) => {
                let invalid = self
                    .entries
                    .iter()
                    .filter(|e| e.invalid().is_some())
                    .count();
                // UNREACHABLE: `strongest` was selected by `invalid().is_some()`.
                // Written as a total match because this crate has no panicking
                // path outside `#[cfg(test)]`.
                let Some(reason) = entry.invalid() else {
                    return Ok(CompleteLedger { ledger: self });
                };
                Err(IncompleteLedger::InvalidContributors {
                    invalid,
                    total,
                    strongest_slot: entry.slot(),
                    strongest_rank: entry.rank().value(),
                    reason: reason.clone(),
                })
            }
        }
    }
}

/// Why a slot obstructs the generation, rather than describing it.
///
/// The label only, never the underlying `io::ErrorKind` or byte counts: a
/// refusal diagnostic is rendered wherever configuration errors are rendered,
/// and the census already carries the detail for a caller that is entitled to
/// it.
fn obstruction_of(outcome: &SourceOutcome) -> Option<&'static str> {
    match outcome {
        // Answers about the configuration. The author's tree genuinely says
        // this, and a generation built over it is complete.
        SourceOutcome::NotConfigured
        | SourceOutcome::Absent
        | SourceOutcome::Duplicate { .. }
        | SourceOutcome::Captured { .. } => None,
        // Failures to obtain a document that may exist. Each one leaves the
        // generation unable to say what the configuration is.
        SourceOutcome::Vanished
        | SourceOutcome::NotAFile
        | SourceOutcome::Unreadable { .. }
        | SourceOutcome::TooLargeAtProbe { .. }
        | SourceOutcome::GrewPastBound { .. } => Some(outcome.kind()),
    }
}

/// Why a ledger is not complete, and therefore cannot resolve or mint layers.
///
/// Two ways to be incomplete, kept apart because they are two different
/// failures with two different fixes: a slot that could not be READ, and a
/// document that could not be DECODED. Both carry the STRONGEST offender,
/// because that is the one whose failure is most consequential — it is the
/// document that was supposed to override every other, and the one whose silent
/// omission would have promoted a weaker document to authoritative.
///
/// Reported as slot, rank and typed label only — never content, for the same
/// reason [`LedgerRefusal::TooManyDocuments`] carries no documents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IncompleteLedger {
    /// A slot could not be read, so what it holds is unknown.
    #[error(
        "{obstructed} of {slots} discovery slots are obstructed;          the strongest is at rank {strongest_rank}: {kind}"
    )]
    Obstructed {
        /// How many slots were obstructed.
        obstructed: usize,
        /// How many slots the census carried in total.
        slots: usize,
        /// The slot of the strongest obstruction.
        strongest_slot: SourceSlot,
        /// The rank of the strongest obstruction.
        strongest_rank: usize,
        /// The obstructing outcome's label.
        kind: &'static str,
    },
    /// Every slot was read, and at least one document is not a contributor.
    #[error(
        "the ledger carries {invalid} invalid contributor(s) of {total};          the strongest is at rank {strongest_rank}: {reason}"
    )]
    InvalidContributors {
        /// How many rows were invalid.
        invalid: usize,
        /// How many rows the ledger carried in total.
        total: usize,
        /// The slot of the strongest invalid contributor.
        strongest_slot: SourceSlot,
        /// The rank of the strongest invalid contributor.
        strongest_rank: usize,
        /// Why the strongest invalid contributor is invalid.
        reason: InvalidContributor,
    },
}

impl IncompleteLedger {
    /// The slot of the strongest offender, whichever kind it is.
    pub fn strongest_slot(&self) -> SourceSlot {
        match self {
            IncompleteLedger::Obstructed { strongest_slot, .. }
            | IncompleteLedger::InvalidContributors { strongest_slot, .. } => *strongest_slot,
        }
    }

    /// The rank of the strongest offender, whichever kind it is.
    pub fn strongest_rank(&self) -> usize {
        match self {
            IncompleteLedger::Obstructed { strongest_rank, .. }
            | IncompleteLedger::InvalidContributors { strongest_rank, .. } => *strongest_rank,
        }
    }
}

/// A ledger in which EVERY captured document became a contributor.
///
/// The only type that can reach [`crate::resolve::resolve`] or
/// [`crate::layer::mint_layers`]. The field is private and
/// [`ConfigLedger::complete`] is the only constructor, so completeness is a
/// property of the type rather than a precondition documented in prose and
/// checked by whoever remembers to. Deleting the check inside `complete` does
/// not open a second door, because there is no second door to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteLedger {
    ledger: ConfigLedger,
}

impl CompleteLedger {
    /// The underlying ledger. Every row is an admitted contributor.
    pub fn ledger(&self) -> &ConfigLedger {
        &self.ledger
    }

    /// Every contributor, weakest first.
    ///
    /// Equivalent to [`ConfigLedger::admitted`] over the same rows, and stated
    /// as its own method because on a complete ledger it is not a filter: it is
    /// all of them.
    pub fn contributors(&self) -> impl Iterator<Item = &LedgerEntry> {
        self.ledger.entries.iter()
    }
}

/// Decode every captured slot in a census into a ledger.
///
/// The bound is applied to the number of CAPTURED slots before any document is
/// decoded, so a census over the cap is refused without allocating a decoded
/// document for any of it.
pub fn admit(census: &SourceCensus, bounds: ConfigBounds) -> Result<ConfigLedger, LedgerRefusal> {
    let captured: Vec<_> = census.captured().collect();
    let bound: MaxDocuments = bounds.documents();
    if captured.len() > bound.value() {
        return Err(LedgerRefusal::TooManyDocuments {
            found: captured.len(),
            bound: bound.value(),
        });
    }

    let mut entries = Vec::with_capacity(captured.len());
    for source in captured {
        // UNREACHABLE: `captured()` yields only `Captured`, so the bytes are
        // always present. Written as a total match rather than an `unwrap`
        // because this crate has no panicking path outside `#[cfg(test)]` and
        // `unwrap`/`expect` are denied here. Skipping is the conservative arm
        // and it is deliberately NOT loud: there is no diagnostic channel on
        // this function, and a slot that reached here without bytes has nothing
        // to contribute anyway. If `captured()` is ever widened, the effect is
        // that the widened outcomes are omitted from the ledger rather than
        // admitted with empty content — which is why the filter and this match
        // must be changed together.
        let bytes = match source.outcome() {
            SourceOutcome::Captured { bytes } => bytes.clone(),
            _ => continue,
        };
        let path = match source.path() {
            Some(path) => path.clone(),
            // A captured slot always has the path it was captured from. If that
            // ever stops being true the document has no provenance to carry, and
            // a contributor without provenance is worse than one that is absent.
            None => continue,
        };
        let state = classify(&bytes, bounds);
        entries.push(LedgerEntry {
            slot: source.slot(),
            rank: source.rank(),
            path,
            state,
        });
    }
    Ok(ConfigLedger {
        census: census.clone(),
        entries,
    })
}

/// Decode one captured document and judge its shape.
fn classify(bytes: &[u8], bounds: ConfigBounds) -> ContributorState {
    match decode(bytes, bounds) {
        Ok(document) => match document.value() {
            ConfigValue::Object(_) => ContributorState::Admitted {
                document,
                bytes: bytes.to_vec(),
            },
            other => {
                ContributorState::Invalid(InvalidContributor::NotAnObject { kind: other.kind() })
            }
        },
        Err(error) => ContributorState::Invalid(InvalidContributor::Undecodable(error)),
    }
}

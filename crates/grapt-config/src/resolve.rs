//! Which contributors supply a path, in what order, and where each came from.
//!
//! This is the second half of **U1b2**. [`crate::ledger`] says which documents
//! became contributors; this module says, for any path, exactly which of them
//! speak to it and in what precedence — and nothing else.
//!
//! # This module deliberately does NOT merge
//!
//! An earlier draft of U1b2 folded the contributors into one merged tree:
//! objects merged key-by-key, everything else replaced wholesale. That
//! contract is OUT OF SCOPE for U1b. Config owns discovery, ordered
//! contributors and layers, stable rank and provenance, explicit invalid state,
//! and complete admission. Folding those contributions into one effective value
//! is a DOMAIN question, and it belongs to the later policy/domain fold that
//! knows what each setting means.
//!
//! The distinction is not bookkeeping. A config-owned recursive merge has to
//! decide, with no knowledge of any setting, whether two objects at a path are
//! one thing to be combined or two things where the nearer replaces the
//! farther; whether an array is a value or a list to append to; whether a
//! `null` clears an inherited key or sets it. Each of those is a per-setting
//! decision, and a crate that answers them uniformly answers most of them
//! wrongly while looking authoritative. This module therefore reports the
//! ordered contributions and lets the layer that knows the domain decide.
//!
//! # The three laws this module exists to hold
//!
//! **Order is the ledger's order, and nothing else is.** Contributions arrive
//! in ascending precedence — farthest slot first, nearest last — exactly as the
//! census produced them. No other rule may reorder them: not document order
//! within a slot, not key order within a document, not which value looks more
//! specific.
//!
//! **Every contribution carries its origin.** A contribution is a value AND the
//! contributor that supplied it. There is no anonymous value here, because a
//! caller folding these has to be able to say afterwards which file it obeyed.
//!
//! **A contribution is reported at the path it was authored at.** Looking up a
//! path descends through objects WITHIN one contributor, which is reading that
//! document, not merging it with any other. Two contributors are never
//! combined to answer a lookup: they are returned as two contributions.
//!
//! # Bounds
//!
//! This module allocates no configuration content. A [`Resolution`] owns
//! nothing beyond one provenance per contributor, and a lookup returns borrowed
//! values from the contributors themselves. There is no merged tree to bound,
//! which is one of the things the ruling bought.

use crate::document::{ConfigValue, Spanned};
use crate::ledger::{CompleteLedger, LedgerEntry};
use crate::source::{ConfigSourcePath, SourceRank, SourceSlot};

/// Which document supplied a value.
///
/// Carries the slot coordinates rather than a document handle: provenance is an
/// answer about ORIGIN, and a reader that could reach the contributing document
/// through it would be able to read a weaker document's bytes out of a
/// contribution that a nearer one overrode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    slot: SourceSlot,
    rank: SourceRank,
    path: ConfigSourcePath,
}

impl Provenance {
    /// The slot the supplying document came from.
    pub fn slot(&self) -> SourceSlot {
        self.slot
    }

    /// The supplying document's slot-derived precedence.
    pub fn rank(&self) -> SourceRank {
        self.rank
    }

    /// The path the supplying document was captured from.
    pub fn path(&self) -> &ConfigSourcePath {
        &self.path
    }

    fn of(entry: &LedgerEntry) -> Self {
        Self {
            slot: entry.slot(),
            rank: entry.rank(),
            path: entry.path().clone(),
        }
    }
}

/// One contributor's answer at a path, and where it came from.
///
/// Borrows the value out of the contributor that authored it. Nothing is
/// copied, combined, or re-shaped on the way here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contribution<'a> {
    value: &'a ConfigValue,
    from: Provenance,
}

impl<'a> Contribution<'a> {
    /// The value this contributor authored at the requested path.
    pub fn value(&self) -> &'a ConfigValue {
        self.value
    }

    /// The contributor that authored it.
    pub fn from(&self) -> &Provenance {
        &self.from
    }
}

/// The ordered contributors a complete ledger describes.
///
/// Holds one entry per contributor, weakest first. It is NOT a configuration
/// value: there is no root object here, because producing one would require the
/// merge this crate does not own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution<'a> {
    contributors: Vec<(Provenance, &'a Spanned<ConfigValue>)>,
}

impl<'a> Resolution<'a> {
    /// How many contributors this resolution carries.
    pub fn len(&self) -> usize {
        self.contributors.len()
    }

    /// Whether there are no contributors at all.
    ///
    /// True for a tree that genuinely holds no configuration document. That is
    /// a complete answer, not a failure: every slot was looked at, and none of
    /// them held a file.
    pub fn is_empty(&self) -> bool {
        self.contributors.is_empty()
    }

    /// Every contributor's whole document, weakest first.
    pub fn contributors(&self) -> impl Iterator<Item = Contribution<'a>> + '_ {
        self.contributors
            .iter()
            .map(|(from, document)| Contribution {
                value: document.value(),
                from: from.clone(),
            })
    }

    /// Every contribution at one path, weakest first.
    ///
    /// A contributor that does not author the path, or that authors a
    /// non-object partway along it, contributes nothing and is simply absent
    /// from the result — it is not reported as a null, because "this file says
    /// nothing here" and "this file says null here" are different statements
    /// and a caller folding them may treat them differently.
    ///
    /// The LAST element is the nearest contributor. This function does not call
    /// it the winner: which contribution prevails is the domain fold's call.
    pub fn contributions_at(&self, path: &[&str]) -> Vec<Contribution<'a>> {
        let mut found = Vec::new();
        for (from, document) in &self.contributors {
            if let Some(value) = descend(document.value(), path) {
                found.push(Contribution {
                    value,
                    from: from.clone(),
                });
            }
        }
        found
    }
}

/// Read one complete ledger as ordered contributors.
///
/// Takes a [`CompleteLedger`] rather than a [`crate::ledger::ConfigLedger`]:
/// an incomplete ledger cannot reach this function at all, so there is no
/// arrangement of arguments in which resolution proceeds over a configuration
/// somebody could not read.
pub fn resolve(ledger: &CompleteLedger) -> Resolution<'_> {
    let mut contributors = Vec::new();
    for entry in ledger.contributors() {
        // UNREACHABLE: every row of a complete ledger is an admitted
        // contributor, and an admitted contributor always kept its document.
        // Written as a total match because this crate has no panicking path
        // outside `#[cfg(test)]`.
        let Some(document) = entry.document() else {
            continue;
        };
        contributors.push((Provenance::of(entry), document));
    }
    Resolution { contributors }
}

/// Follow a path of object keys WITHIN one contributor's document.
///
/// Reading one document, never combining two. `None` when any segment is
/// missing or when a segment would have to descend through a non-object.
fn descend<'a>(value: &'a ConfigValue, path: &[&str]) -> Option<&'a ConfigValue> {
    let mut node = value;
    for segment in path {
        let ConfigValue::Object(fields) = node else {
            return None;
        };
        node = fields
            .iter()
            .find(|(key, _)| key.value().as_str() == *segment)
            .map(|(_, field)| field.value())?;
    }
    Some(node)
}

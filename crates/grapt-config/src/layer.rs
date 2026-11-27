//! The bridge that mints `grapt_policy::ConfigLayer` values.
//!
//! This is the seam U1a and U1b1 both named and deliberately did not build: the
//! point where source custody stops describing documents and starts feeding the
//! effective-authority graph. It is the only place in this crate that depends
//! on `grapt-policy`, and the dependency-direction gate's allowance for that
//! crate exists for exactly this module.
//!
//! # The three laws this module exists to hold
//!
//! **Only a COMPLETE ledger mints anything at all.** This function names
//! [`CompleteLedger`], which cannot be constructed while any slot is
//! unreadable or any captured document failed to decode. Minting the readable
//! subset of a broken configuration is the specific failure this refuses: a
//! syntax error in the NEAREST file would otherwise mint layers for every
//! weaker file and hand the authority graph a configuration in which the
//! document the author edited last is the only one absent. The ledger keeps
//! the invalid row; the graph never sees a partial generation.
//!
//! **The layer's rank is the SLOT-DERIVED census rank, unchanged.** The mint
//! binds the rank into the contribution identity, so an identity minted for
//! rank 5 does not verify as the same contribution at rank 6. Renumbering
//! layers here — compacting them, or numbering them by position in the ledger —
//! would produce identities that no longer name the slot the document actually
//! came from, and precedence in the graph would stop matching precedence in the
//! census.
//!
//! **The minted content is the CAPTURED BYTES, not a re-encoding.** The ledger
//! keeps each admitted contributor's canonical bytes for this reason. Minting
//! over a re-serialization of the decoded document would key the identity to
//! this crate's formatting choices rather than to what the author wrote, so two
//! byte-different documents that happen to decode alike would collide and one
//! byte-identical document would change identity whenever the encoder changed.
//!
//! # Why the authority is injected
//!
//! `grapt_policy::ContributionAuthority` is passed in and never constructed
//! here. An authority this crate minted for itself would be a per-process key,
//! and a per-process key makes every identity unstable across restarts — the
//! precise failure `grapt-policy` refuses in its own ruling.

use grapt_policy::{ConfigLayer, ConfigSource, ContributionAuthority, Secret};

use crate::ledger::CompleteLedger;
use crate::source::SourceSlot;

/// The `ConfigSource` class a discovery slot contributes as.
///
/// Total over [`SourceSlot`] and written as a match rather than a default so a
/// new slot kind cannot silently inherit a neighbour's class: a slot whose
/// class was guessed would be labelled in the authority graph as something
/// nobody decided it was.
fn source_of(slot: SourceSlot) -> ConfigSource {
    match slot {
        // The caller's global configuration directory.
        SourceSlot::Global => ConfigSource::GlobalFile,
        // Every ancestor slot, including the start directory, is a project
        // file: they are the same class of authority at different distances,
        // and the distance is already carried by the rank.
        SourceSlot::Ancestor { .. } => ConfigSource::ProjectFile,
    }
}

/// Mint one `ConfigLayer` per contributor, in ascending precedence.
///
/// Every row of a complete ledger is a contributor, so this mints one layer per
/// row and skips nothing. The returned order is the ledger's, which is the
/// census's: weakest first.
pub fn mint_layers(ledger: &CompleteLedger, authority: &ContributionAuthority) -> Vec<ConfigLayer> {
    let mut layers = Vec::new();
    for entry in ledger.contributors() {
        // UNREACHABLE: every row of a complete ledger is an admitted
        // contributor, and a contributor always kept the bytes it decoded from.
        // Written as a total match because this crate has no panicking path
        // outside `#[cfg(test)]`.
        let Some(bytes) = entry.bytes() else {
            continue;
        };
        layers.push(ConfigLayer::new(
            authority,
            entry.rank().value() as u64,
            source_of(entry.slot()),
            &Secret::new(bytes.to_vec()),
        ));
    }
    layers
}

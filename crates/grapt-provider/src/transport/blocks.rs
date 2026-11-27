//! Text/reasoning block identity and ordinal allocation — provider-neutral.
//!
//! Shared with the ChatGPT/Codex route. Corrupted block identity is
//! a property of the normalized alphabet, not of one vendor: any adapter that
//! can have a text and a reasoning block open at once can corrupt block
//! identity the same way. Anthropic's `content_block_*` events make that
//! interleaving explicit, so the shared allocator is what stops the defect
//! being reintroduced on a second route.

use crate::fragment::BlockOrdinal;

/// Which blocks are open, and under which ordinal each one lives.
///
/// # A block keeps its own ordinal for its whole life
///
/// The validator deliberately permits one text block and one reasoning block
/// open at the same time, so a single shared identity would attach a later
/// text delta to the block that opened last and close both under the same
/// ordinal — corrupted block identity, not a cosmetic defect.
///
/// The governed semantics are in the master's `ContentOrdinal` row — "Each
/// text, reasoning, tool and opaque opening appends exactly one item at the
/// next `ContentOrdinal` in `NormalizedContentSequence`; later deltas cannot
/// reorder it." So an ordinal is allocated **once, at opening**, and belongs to
/// that block until it closes. Each kind therefore holds its own allocation.
#[derive(Debug, Default)]
pub(crate) struct BlockState {
    next_ordinal: u32,
    text: Option<BlockOrdinal>,
    reasoning: Option<BlockOrdinal>,
}

impl BlockState {
    pub(crate) fn allocate(&mut self) -> BlockOrdinal {
        let ordinal = BlockOrdinal::new(self.next_ordinal);
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        ordinal
    }

    /// The open text block, opening one if none is open. The `bool` is whether
    /// this call opened it, so the caller knows to emit `TextOpened`.
    pub(crate) fn open_text(&mut self) -> (BlockOrdinal, bool) {
        match self.text {
            Some(ordinal) => (ordinal, false),
            None => {
                let ordinal = self.allocate();
                self.text = Some(ordinal);
                (ordinal, true)
            }
        }
    }

    pub(crate) fn open_reasoning(&mut self) -> (BlockOrdinal, bool) {
        match self.reasoning {
            Some(ordinal) => (ordinal, false),
            None => {
                let ordinal = self.allocate();
                self.reasoning = Some(ordinal);
                (ordinal, true)
            }
        }
    }

    pub(crate) fn close_text(&mut self) -> Option<BlockOrdinal> {
        self.text.take()
    }

    pub(crate) fn close_reasoning(&mut self) -> Option<BlockOrdinal> {
        self.reasoning.take()
    }
}

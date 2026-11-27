//! Normalized content ordering. Law (foundational report row `content`;
//! supersession matrix "Ordered content" row): one closed ordered sequence of
//! text, reasoning, tool and opaque-item references preserves interleaving.
//! "Parallel per-kind arrays cannot reconstruct interleaving. Reasoning
//! signature/tool pairing and truncation retain their exact position."
//!
//! F1A owns the ordering vocabulary: the per-item ordinal, the closed kind, and
//! the sequence that keeps them in one order. What each item REFERENCES (the
//! exact artifact/text/reasoning-signature payload) is bound by the provider/
//! event owners; here an item carries its position and kind so replay and
//! truncation never lose interleaving.

use crate::artifact::ArtifactRef;
use crate::subagent::ToolCallId;

/// A non-negative position within a [`NormalizedContentSequence`]. Zero is the
/// first item; ordinals are dense and strictly increasing within a sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentOrdinal(u64);

impl ContentOrdinal {
    pub const FIRST: ContentOrdinal = ContentOrdinal(0);

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// The closed kind of one content item. Reasoning keeps its distinct position
/// from text (a signature is never folded into prose), and a tool item is paired
/// to its exact call so interleaving survives replay/truncation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContentKind {
    /// Assistant text.
    Text,
    /// Model reasoning, kept positionally distinct from text.
    Reasoning,
    /// A tool call, paired to its exact call identity.
    Tool { call: ToolCallId },
    /// An opaque provider item whose position must be preserved even though its
    /// content is not interpreted here.
    Opaque,
}

/// One positioned content item: its ordinal and kind. The reference to the
/// item's actual payload is owned by the provider/event layer; this type fixes
/// its ORDER within the sequence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedContentItem {
    ordinal: ContentOrdinal,
    kind: ContentKind,
    payload: ArtifactRef,
}

impl NormalizedContentItem {
    /// Every item carries its ordered payload BY REFERENCE.
    ///
    /// Law: parallel per-kind arrays cannot reconstruct interleaving, and an
    /// item that names only its kind and position cannot be replayed at all.
    /// The payload is a bounded [`ArtifactRef`], never inline bytes, so the
    /// sequence stays byte-bounded while remaining replayable.
    pub fn new(ordinal: ContentOrdinal, kind: ContentKind, payload: ArtifactRef) -> Self {
        Self {
            ordinal,
            kind,
            payload,
        }
    }

    pub fn ordinal(&self) -> ContentOrdinal {
        self.ordinal
    }

    pub fn kind(&self) -> &ContentKind {
        &self.kind
    }

    pub fn payload(&self) -> &ArtifactRef {
        &self.payload
    }
}

/// One closed ordered sequence. Items are held in a single order rather than in
/// parallel per-kind arrays, so text/reasoning/tool interleaving is exact.
/// Construction rejects out-of-order or duplicated ordinals — the sequence is
/// dense and strictly increasing from [`ContentOrdinal::FIRST`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedContentSequence {
    items: Vec<NormalizedContentItem>,
}

impl NormalizedContentSequence {
    /// Build a sequence from items already in order. Returns `None` if the
    /// ordinals are not the dense strictly-increasing run `0,1,2,…`, so a caller
    /// cannot smuggle a gap or reorder past this constructor.
    pub fn new(items: Vec<NormalizedContentItem>) -> Option<Self> {
        for (index, item) in items.iter().enumerate() {
            if item.ordinal().value() != index as u64 {
                return None;
            }
        }
        Some(Self { items })
    }

    pub fn items(&self) -> &[NormalizedContentItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::artifact::{ArtifactDigest, ArtifactId};

    fn payload(ordinal: u64) -> ArtifactRef {
        // Distinct per position: an item's payload reference is part of what
        // makes the sequence replayable, not decoration.
        match (
            ArtifactId::new(format!("item-{ordinal}")),
            ArtifactDigest::new(format!("digest-{ordinal}")),
        ) {
            (Some(id), Some(digest)) => ArtifactRef::new(id, 8, digest),
            _ => unreachable!("non-empty artifact coordinates"),
        }
    }

    fn item(ordinal: u64, kind: ContentKind) -> NormalizedContentItem {
        NormalizedContentItem::new(ContentOrdinal::new(ordinal), kind, payload(ordinal))
    }

    #[test]
    fn every_item_carries_its_own_ordered_payload_reference(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let first = item(0, ContentKind::Text);
        let second = item(1, ContentKind::Text);
        assert_eq!(first.ordinal(), ContentOrdinal::FIRST);
        assert_ne!(
            first.payload(),
            second.payload(),
            "position and payload travel together, so replay cannot swap them"
        );
        assert!(matches!(first.kind(), ContentKind::Text));
        Ok(())
    }

    #[test]
    fn a_sequence_preserves_interleaving_and_rejects_gaps() -> Result<(), Box<dyn std::error::Error>>
    {
        let call = ToolCallId::new("call_1").ok_or("tool call id")?;
        let ok = NormalizedContentSequence::new(vec![
            item(0, ContentKind::Text),
            item(1, ContentKind::Reasoning),
            item(2, ContentKind::Tool { call }),
            item(3, ContentKind::Text),
        ]);
        assert!(ok.is_some());
        assert_eq!(ok.ok_or("sequence")?.len(), 4);

        // A gap or reorder is refused by construction.
        assert!(NormalizedContentSequence::new(vec![
            item(0, ContentKind::Text),
            item(2, ContentKind::Text),
        ])
        .is_none());
        assert!(NormalizedContentSequence::new(vec![
            item(1, ContentKind::Text),
            item(0, ContentKind::Text),
        ])
        .is_none());
        Ok(())
    }
}

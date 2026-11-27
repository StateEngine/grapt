//! Resource budgets for public open metadata and read-only tool inputs.
use super::*;

#[derive(Debug)]
pub(super) struct OpenBudget {
    maximum_nodes: usize,
    maximum_keys: usize,
    maximum_items: usize,
    maximum_depth: usize,
    maximum_scalar_bytes: usize,
    maximum_encoded_bytes: usize,
    nodes: usize,
    keys: usize,
    items: usize,
    scalar_bytes: usize,
    encoded_bytes: usize,
}

impl OpenBudget {
    pub(super) fn new() -> Self {
        Self {
            maximum_nodes: MAX_METADATA_NODES,
            maximum_keys: MAX_METADATA_KEYS,
            maximum_items: MAX_METADATA_NODES,
            maximum_depth: MAX_METADATA_DEPTH,
            maximum_scalar_bytes: MAX_METADATA_SCALAR_BYTES,
            maximum_encoded_bytes: MAX_METADATA_BYTES,
            nodes: 0,
            keys: 0,
            items: 0,
            scalar_bytes: 0,
            encoded_bytes: 0,
        }
    }

    /// Read-only history/tool progress can carry the provider's 1024 native
    /// search sources. Keep their collection budget separate from metadata.
    pub(super) fn tool_input() -> Self {
        Self {
            maximum_nodes: 8192,
            maximum_keys: 4096,
            maximum_items: 2048,
            maximum_depth: 32,
            maximum_scalar_bytes: 256 * 1024,
            maximum_encoded_bytes: 256 * 1024,
            ..Self::new()
        }
    }

    fn add<E: DeError>(
        slot: &mut usize,
        amount: usize,
        maximum: usize,
        overflow: &'static str,
        exceeded: &'static str,
    ) -> Result<(), E> {
        *slot = slot
            .checked_add(amount)
            .ok_or_else(|| E::custom(overflow))?;
        if *slot > maximum {
            return Err(E::custom(exceeded));
        }
        Ok(())
    }

    pub(super) fn node<E: DeError>(&mut self, depth: usize) -> Result<(), E> {
        if depth > self.maximum_depth {
            return Err(E::custom("metadata exceeds its depth bound"));
        }
        Self::add(
            &mut self.nodes,
            1,
            self.maximum_nodes,
            "metadata node count overflow",
            "metadata exceeds its node bound",
        )
    }

    pub(super) fn key<E: DeError>(&mut self) -> Result<(), E> {
        Self::add(
            &mut self.keys,
            1,
            self.maximum_keys,
            "metadata key count overflow",
            "metadata exceeds its key bound",
        )
    }

    pub(super) fn item<E: DeError>(&mut self) -> Result<(), E> {
        Self::add(
            &mut self.items,
            1,
            self.maximum_items,
            "metadata item count overflow",
            "metadata exceeds its item bound",
        )
    }

    pub(super) fn scalar<E: DeError>(&mut self, bytes: usize) -> Result<(), E> {
        Self::add(
            &mut self.scalar_bytes,
            bytes,
            self.maximum_scalar_bytes,
            "metadata scalar byte count overflow",
            "metadata exceeds its scalar-byte bound",
        )
    }

    pub(super) fn encoded<E: DeError>(&mut self, bytes: usize) -> Result<(), E> {
        Self::add(
            &mut self.encoded_bytes,
            bytes,
            self.maximum_encoded_bytes,
            "metadata encoded byte count overflow",
            "metadata exceeds its encoded-byte bound",
        )
    }
}

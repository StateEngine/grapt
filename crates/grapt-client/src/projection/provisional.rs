//! Live blocks a snapshot carries (B3 S2c). After the durable install at P,
//! each opens as an identity-tagged slice holding the text every subscriber
//! was offered, unless the install already holds the block durably: the
//! producer can commit a block before its end leaves the live registry, and
//! durable truth wins.
use super::*;

impl SessionView {
    /// Whether the installed history already holds block `ordinal` of `kind`
    /// of assistant `message`, as that message's `ordinal`-th slice of `kind`.
    pub(crate) fn holds_durably(
        &self,
        message: &crate::wire::MessageWireId,
        kind: SliceKind,
        ordinal: u64,
    ) -> bool {
        self.durable_bytes(message, kind, ordinal).is_some()
    }

    /// The bytes the installed history holds for block `ordinal` of `kind` of
    /// assistant `message`, if it holds that block.
    pub(crate) fn durable_bytes(
        &self,
        message: &crate::wire::MessageWireId,
        kind: SliceKind,
        ordinal: u64,
    ) -> Option<usize> {
        let ordinal = usize::try_from(ordinal).ok()?;
        let mut start = self.older_len;
        for (row, count) in &self.tail_rows {
            let end = start.saturating_add(*count);
            if row == message {
                let slice = self
                    .slices
                    .get(start..end)?
                    .iter()
                    .filter(|slice| slice.kind == kind)
                    .nth(ordinal)?;
                return Some(slice.text.len().saturating_add(slice.truncated_bytes));
            }
            start = end;
        }
        None
    }

    /// Whether a positioned delta for a block that is not open belongs to a
    /// message the installed snapshot holds durably. The snapshot reopens every
    /// block still live at P, so such a block ended at or before P: a delta the
    /// stream retained from before the read is superseded by the durable row.
    pub(super) fn superseded_delta(&self, envelope: &EventEnvelope, kind: SliceKind) -> bool {
        wire_identity(envelope, kind)
            .is_ok_and(|(message, _)| self.tail_rows.iter().any(|(row, _)| row == &message))
    }

    /// Open one live block from a snapshot: identity-tagged, holding its
    /// offered text, so later deltas apply by offset after it.
    pub(crate) fn open_provisional(
        &mut self,
        kind: SliceKind,
        identity: (crate::wire::MessageWireId, u64),
        text: &str,
        bounds: Bounds,
    ) -> Result<Applied, ProjectionError> {
        if self.holds_durably(&identity.0, kind, identity.1) {
            return Ok(Applied::Ignored);
        }
        if self.adopted == Some(kind) {
            self.seal_unfinished();
        }
        if self.open_by_identity(kind, &identity).is_some() {
            return Err(ProjectionError::OverlappingOpen { kind });
        }
        // Kept like a live delta: on a character boundary, the rest counted.
        let mut cut = text.len().min(bounds.max_slice_bytes);
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let kept = text.get(..cut).unwrap_or_default().to_owned();
        self.push_open(kind, identity, kept, text.len() - cut, bounds);
        // The whole offered text is admitted, so the digest covers it all.
        let ordinal = self.slices.last().map(|slice| slice.ordinal);
        if let Some(wired) = ordinal.and_then(|ordinal| self.slice_wires.get_mut(&ordinal)) {
            wired.digest.update(text.as_bytes());
        }
        Ok(Applied::Changed)
    }
}

impl SessionView {
    /// Every open, identity-tagged slice and the bytes it has received.
    pub(crate) fn live_blocks(&self) -> Vec<(SliceKind, (crate::wire::MessageWireId, u64), usize)> {
        self.slices
            .iter()
            .filter(|slice| !slice.closed)
            .filter_map(|slice| {
                let identity = self.slice_wires.get(&slice.ordinal)?.identity.clone();
                let received = slice.text.len().saturating_add(slice.truncated_bytes);
                Some((slice.kind, identity, received))
            })
            .collect()
    }

    /// The bytes an open block holds now, if it is open under that identity.
    pub(crate) fn live_bytes(
        &self,
        kind: SliceKind,
        identity: &(crate::wire::MessageWireId, u64),
    ) -> Option<usize> {
        let index = self.open_by_identity(kind, identity)?;
        let slice = self.slices.get(index)?;
        Some(slice.text.len().saturating_add(slice.truncated_bytes))
    }
}

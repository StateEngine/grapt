//! Native composer commands share the selected-session client.
use super::*;

impl<C: Connection> Client<C> {
    /// Inspect the selected Session's last capture; never refresh instructions from disk.
    pub fn instruction_context(&mut self) -> Result<Vec<String>, ClientError> {
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        let bytes = self.ingest(
            Operation::InstructionInspect,
            Some(&session),
            None,
            None,
            None,
            None,
        )?;
        Ok(crate::wire::decode_instruction_context(&bytes)?)
    }

    /// Save the selected Session name; picker refresh is a separate read.
    pub fn rename(&mut self, title: &str) -> Result<(), ClientError> {
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        self.rename_session(&session, title)
    }

    /// Admit compaction; it runs before the next message.
    pub fn compact(&mut self) -> Result<(), ClientError> {
        self.compact_with_instructions(None)
    }

    /// Optional instructions apply to this durable compaction request only.
    pub fn compact_with_instructions(
        &mut self,
        instructions: Option<&str>,
    ) -> Result<(), ClientError> {
        let Some(session) = self.selected.clone() else {
            return Err(ClientError::NoSelection);
        };
        let body = crate::wire::encode_compact_with_instructions(instructions)?;
        let admitted = self.ingest(
            Operation::SessionCompact,
            Some(&session),
            None,
            Some(body),
            None,
            None,
        )?;
        crate::wire::decode_compacted(&admitted)?;
        Ok(())
    }

    /// Record nested tool runs lost with their process as unknown; how many.
    pub fn reconcile_effects(&mut self) -> Result<u64, ClientError> {
        let Some(session) = self.selected.clone() else {
            return Err(ClientError::NoSelection);
        };
        let body = crate::wire::encode_reconcile()?;
        let reconciled = self.ingest(
            Operation::SessionReconcile,
            Some(&session),
            None,
            Some(body),
            None,
            None,
        )?;
        Ok(crate::wire::decode_reconciled(&reconciled)?)
    }
}

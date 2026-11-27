//! Strictly-decoded READS of the operation table.
//!
//! Separated from `service.rs` because they are the half that takes no
//! transaction: every one of them answers a question about a stored operation
//! and writes nothing. Keeping them beside the transitions made one file carry
//! two different responsibilities, and the file grew past this workspace's
//! module ceiling saying so.
//!
//! They still fail closed exactly as the transitions do — each goes through the
//! same strict `decode_row`, so a malformed persisted value is `Corruption`
//! here too and no caller ever acts on a raw string.

use super::*;

/// Immutable child routing and its current durable lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildOperationContext {
    pub operation: SubagentOperationId,
    pub parent: SessionId,
    pub mode: SubagentMode,
    pub status: OperationStatus,
}

impl SubagentService {
    /// The generation THIS runtime should start operations under.
    ///
    /// One past the highest any operation has ever recorded, so a process that
    /// replaces another cannot reuse its predecessor's generation and cannot be
    /// mistaken for it by [`Self::settle_dead_generation_on`]. Derived from the
    /// durable rows rather than kept as separate state: a counter that could
    /// drift from the rows it describes would be a second, disagreeable truth.
    pub fn next_generation_on(store: &EventStore) -> Result<RuntimeGeneration, SubagentError> {
        let connection = store.connection();
        let installed: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'subagent_operation')",
            [],
            |row| row.get(0),
        )?;
        if !installed {
            return Ok(RuntimeGeneration::new(1));
        }
        let highest: Option<i64> = connection.query_row(
            "SELECT MAX(MAX(COALESCE(generation, 0), COALESCE(run_generation, 0)))
             FROM subagent_operation",
            [],
            |row| row.get(0),
        )?;
        let highest = u64::try_from(highest.unwrap_or(0))
            .unwrap_or(0)
            .max(followup::highest_generation(connection)?);
        Ok(RuntimeGeneration::new(highest.saturating_add(1)))
    }

    /// The operation this Session is the CHILD of, if it is one.
    ///
    /// This is the read that lets a caller ask "is this Session somebody's
    /// subagent child?" before treating it as an ordinary Session. The
    /// admission already records `child_session_id`; nothing until now could
    /// ask the question in that direction, which is why a child could be
    /// addressed exactly like any other Session.
    ///
    /// A child has one initial operation and may have explicitly bound
    /// successors. Every retained link is checked; unrelated duplicate rows,
    /// missing predecessors and malformed state refuse instead of selecting
    /// an arbitrary latest operation.
    ///
    /// Reads against a store where no operation has ever been admitted: the
    /// table is installed by [`Self::ensure_schema_on`] at the first admission,
    /// and its absence means precisely that no Session is a child.
    pub fn child_operation_on(
        store: &EventStore,
        child: &SessionId,
    ) -> Result<Option<(SubagentOperationId, OperationStatus)>, SubagentError> {
        Ok(
            Self::child_context_on(store, child)?
                .map(|context| (context.operation, context.status)),
        )
    }

    /// Resolve a child through the durable owner, including its immutable parent
    /// and delivery mode. Unlinked child bindings and malformed runs refuse.
    pub fn child_context_on(
        store: &EventStore,
        child: &SessionId,
    ) -> Result<Option<ChildOperationContext>, SubagentError> {
        Self::child_context_within(store.connection(), child)
    }

    pub(super) fn child_context_within(
        connection: &rusqlite::Connection,
        child: &SessionId,
    ) -> Result<Option<ChildOperationContext>, SubagentError> {
        let installed: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'subagent_operation')",
            [],
            |row| row.get(0),
        )?;
        if !installed {
            return Ok(None);
        }
        Ok(followup::read_chain(connection, child)?.map(|chain| chain.current))
    }

    /// The strictly-decoded status and delivery of an operation. Delegates to
    /// the single strict decoder, so a malformed persisted target surfaces as
    /// `Corruption` here too.
    pub fn get(
        &self,
        operation: &SubagentOperationId,
    ) -> Result<Option<(OperationStatus, DeliveryStatus)>, SubagentError> {
        let Some(row) = Self::decode_row(self.store.connection(), operation)? else {
            return Ok(None);
        };
        let delivery = match row.delivery {
            DecodedDelivery::Unclaimed => DeliveryStatus::Unclaimed,
            DecodedDelivery::Claimed {
                id,
                route,
                delivered,
                settlement,
                acknowledged,
                cleaned,
                ..
            } => {
                let settled = settlement.ok_or_else(|| SubagentError::Corruption {
                    operation: row.subagent.clone(),
                    reason: "delivered state has no durable settlement".into(),
                });
                if cleaned {
                    DeliveryStatus::Cleaned(id, route, settled?)
                } else if acknowledged {
                    DeliveryStatus::Acknowledged(id, route, settled?)
                } else if delivered {
                    match settled? {
                        DeliverySettlement::Completed => DeliveryStatus::Delivered(id, route),
                        DeliverySettlement::Failed => DeliveryStatus::Failed(id, route),
                        DeliverySettlement::Quarantined => DeliveryStatus::Quarantined(id, route),
                    }
                } else {
                    DeliveryStatus::Delivering(id, route)
                }
            }
        };
        Ok(Some((row.status, delivery)))
    }

    /// Strictly decode the remaining persisted admission identities — model
    /// (all-or-none), tool call, and effective graph — to their newtypes. Any
    /// malformed value is `Corruption`, so no transition or delivery proceeds on
    /// a corrupt admission.
    pub(super) fn validate_admission_identities(
        conn: &rusqlite::Connection,
        operation: &SubagentOperationId,
    ) -> Result<(), SubagentError> {
        let corrupt = |reason: String| SubagentError::Corruption {
            operation: operation.clone(),
            reason,
        };
        let (provider, model, variant, tool_call, effective_graph): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ) = conn.query_row(
            "SELECT model_provider, model_id, model_variant, tool_call_id, effective_graph_id
             FROM subagent_operation WHERE operation_id = ?1",
            [operation.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        match (provider, model) {
            (Some(provider), Some(model)) => {
                ProviderId::new(provider)
                    .ok_or_else(|| corrupt("model provider is empty".into()))?;
                ModelId::new(model).ok_or_else(|| corrupt("model id is empty".into()))?;
                if let Some(variant) = variant {
                    ModelVariantId::new(variant)
                        .ok_or_else(|| corrupt("model variant is empty".into()))?;
                }
            }
            (None, None) => {
                if variant.is_some() {
                    return Err(corrupt("model variant without a model".into()));
                }
            }
            _ => return Err(corrupt("model is neither fully present nor absent".into())),
        }
        if let Some(tool_call) = tool_call {
            ToolCallId::new(tool_call).ok_or_else(|| corrupt("tool call id is empty".into()))?;
        }
        EffectiveGraphId::new(effective_graph)
            .ok_or_else(|| corrupt("effective graph id is empty".into()))?;
        Ok(())
    }
}

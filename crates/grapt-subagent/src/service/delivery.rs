//! Claim, deliver and acknowledge through one durable subagent owner.

use super::*;

impl SubagentService {
    /// Publish at most eight finished background operations. Stable identities
    /// let a crash between claim and delivery resume that exact handoff.
    pub fn publish_background_on(
        store: &mut EventStore,
        parent: &SessionId,
        limits: DeliveryLimits,
    ) -> Result<usize, SubagentError> {
        let installed: bool = store.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='subagent_operation')",
            [], |row| row.get(0),
        )?;
        if !installed {
            return Ok(0);
        }
        let mut query = store.connection().prepare(
            "SELECT CASE WHEN length(CAST(operation_id AS BLOB))<=512 THEN operation_id END,
             time_updated FROM subagent_operation WHERE parent_session_id=?1
             AND mode='background' AND status IN ('completed','failed','cancelled','ambiguous')
             AND delivery!='delivered' ORDER BY operation_id LIMIT 8",
        )?;
        let rows = query
            .query_map([parent.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(query);
        let mut published = 0;
        for (raw, created_ms) in rows {
            let operation = SubagentOperationId::new(raw.clone()).ok_or_else(|| {
                SubagentError::CorruptOperationId {
                    raw,
                    reason: "invalid background delivery address".into(),
                }
            })?;
            let row = Self::decode_bounded_row(store.connection(), &operation)?;
            let corrupt = |reason: &str| SubagentError::Corruption {
                operation: operation.clone(),
                reason: reason.into(),
            };
            if row.parent != *parent || row.mode != SubagentMode::Background || created_ms < 0 {
                return Err(corrupt(
                    "background delivery has foreign coordinates or time",
                ));
            }
            let generation = row
                .run_generation
                .ok_or_else(|| corrupt("terminal has no run generation"))?;
            let claim = match row.delivery {
                DecodedDelivery::Unclaimed => DeliveryClaim {
                    id: DeliveryClaimId::new(format!("delivery_{}", operation.as_str()))
                        .ok_or_else(|| corrupt("delivery claim identity"))?,
                    input: format!("msg_delivery_{}", operation.as_str())
                        .parse()
                        .map_err(|_| corrupt("delivery input identity"))?,
                    route: ClaimRoute::OperationDelivery,
                    parent: parent.clone(),
                    generation,
                },
                DecodedDelivery::Claimed {
                    id,
                    route,
                    parent,
                    input,
                    generation,
                    ..
                } => DeliveryClaim {
                    id,
                    route,
                    parent,
                    input,
                    generation,
                },
            };
            let _outcome = Self::claim_on(store, &operation, &claim, created_ms)?;
            let event: EventId = format!("evt_delivery_{}", operation.as_str())
                .parse()
                .map_err(|_| corrupt("delivery event identity"))?;
            if Self::deliver_on(
                store, limits, &operation, &claim.id, generation, &event, created_ms,
            )? == CasOutcome::Applied
            {
                published += 1;
            }
        }
        Ok(published)
    }

    /// Release inbox capacity only for a result already captured with a permit.
    pub fn acknowledge_presented_on(
        store: &mut EventStore,
        parent: &SessionId,
    ) -> Result<(), SubagentError> {
        for (receipt, created_ms) in
            grapt_inbox::operation_mail::OperationMailBatch::captured_receipts(store, parent)
                .map_err(|reason| SubagentError::MailPresentation { reason })?
        {
            let row = Self::decode_bounded_row(store.connection(), &receipt.operation)?;
            if row.parent != *parent {
                return Err(SubagentError::ClaimParentMismatch {
                    operation: receipt.operation,
                });
            }
            Self::acknowledge_on(
                store,
                &receipt.operation,
                &receipt.claim,
                receipt.generation,
                created_ms,
            )?;
        }
        Ok(())
    }
    /// Claim the delivery of a settled, undelivered operation, binding the
    /// EXACT delivery coordinates. Exactly-once: the first claim wins; any later
    /// claim loses. The claim route MUST match the operation's mode.
    pub fn claim(
        &mut self,
        operation: &SubagentOperationId,
        claim: &DeliveryClaim,
        updated_ms: i64,
    ) -> Result<ClaimOutcome, SubagentError> {
        Self::claim_on(&mut self.store, operation, claim, updated_ms)
    }

    /// The same transition against the application's existing store owner.
    pub fn claim_on(
        store: &mut EventStore,
        operation: &SubagentOperationId,
        claim: &DeliveryClaim,
        updated_ms: i64,
    ) -> Result<ClaimOutcome, SubagentError> {
        let tx = store.governed_transaction()?;
        // Bind the claim to the operation's IMMUTABLE admission facts before any
        // write: the route must match the mode (background -> operation delivery,
        // foreground -> tool-result), and the claim's parent must be the
        // operation's own parent route — a claim can never target another
        // session. Both mismatches are typed errors with zero writes.
        if let Some(row) = Self::decode_row(&tx, operation)? {
            if claim.route != row.mode.route() {
                return Err(SubagentError::MisroutedClaim {
                    operation: operation.clone(),
                    route: claim.route.as_str(),
                    mode: row.mode.as_str(),
                });
            }
            if claim.parent != row.parent {
                return Err(SubagentError::ClaimParentMismatch {
                    operation: operation.clone(),
                });
            }
            let terminal_generation =
                row.run_generation
                    .ok_or_else(|| SubagentError::Corruption {
                        operation: operation.clone(),
                        reason: "settled operation has no run generation".into(),
                    })?;
            if claim.generation != terminal_generation {
                return Err(SubagentError::StaleGeneration {
                    operation: operation.clone(),
                    presented: claim.generation.value(),
                    terminal: terminal_generation.value(),
                });
            }
        }
        let n = tx.execute(
            "UPDATE subagent_operation
             SET delivery = 'delivering', claim_id = ?2, claim_route = ?3,
                 target_parent = ?4, target_input = ?5, claim_generation = ?6,
                 time_updated = ?7
             WHERE operation_id = ?1
               AND status IN ('completed', 'failed', 'cancelled', 'ambiguous')
               AND run_generation = ?6
               AND delivery = 'unclaimed'",
            rusqlite::params![
                operation.as_str(),
                claim.id.as_str(),
                claim.route.as_str(),
                claim.parent.as_str(),
                claim.input.as_str(),
                generation_to_i64(claim.generation)?,
                updated_ms,
            ],
        )?;
        tx.commit()?.discard();
        Ok(if n == 1 {
            ClaimOutcome::Won
        } else {
            ClaimOutcome::Lost
        })
    }

    /// Deliver a background result as an artifact-backed operation-delivery
    /// fact. It is never a user input and carries no steer/queue mode.
    pub fn deliver(
        &mut self,
        operation: &SubagentOperationId,
        claim_id: &DeliveryClaimId,
        generation: RuntimeGeneration,
        event_id: &EventId,
        created_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        Self::deliver_on(
            &mut self.store,
            self.limits,
            operation,
            claim_id,
            generation,
            event_id,
            created_ms,
        )
    }

    /// The same transition against the application's existing store owner.
    pub fn deliver_on(
        store: &mut EventStore,
        limits: DeliveryLimits,
        operation: &SubagentOperationId,
        claim_id: &DeliveryClaimId,
        generation: RuntimeGeneration,
        event_id: &EventId,
        created_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        // The parent session is only known after the row read below, so the
        // store's held tokens are snapshotted before the transaction borrows it.
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        let outcome = match Self::decode_row(&tx, operation)? {
            None => CasOutcome::NotLive,
            Some(row) => {
                let DecodedRow {
                    run_generation,
                    status,
                    delivery,
                    ..
                } = row;
                match delivery {
                    DecodedDelivery::Unclaimed => CasOutcome::NotLive,
                    // A stale/foreign claim id cannot deliver.
                    DecodedDelivery::Claimed { id, .. } if id.as_str() != claim_id.as_str() => {
                        CasOutcome::NotLive
                    }
                    DecodedDelivery::Claimed {
                        generation: claimed_generation,
                        ..
                    } if claimed_generation != generation => CasOutcome::NotLive,
                    DecodedDelivery::Claimed {
                        delivered: true, ..
                    } => CasOutcome::AlreadyTerminal,
                    DecodedDelivery::Claimed { route, .. }
                        if route != ClaimRoute::OperationDelivery =>
                    {
                        return Err(SubagentError::WrongRouteForDelivery {
                            operation: operation.clone(),
                            route: route.as_str(),
                        });
                    }
                    DecodedDelivery::Claimed { .. } => {
                        let terminal_generation =
                            run_generation.ok_or_else(|| SubagentError::Corruption {
                                operation: operation.clone(),
                                reason: "claimed terminal has no run generation".into(),
                            })?;
                        if terminal_generation != generation {
                            return Err(SubagentError::StaleGeneration {
                                operation: operation.clone(),
                                presented: generation.value(),
                                terminal: terminal_generation.value(),
                            });
                        }
                        match &status {
                            OperationStatus::Terminal(
                                SubagentTerminal::Completed { .. }
                                | SubagentTerminal::Failed { .. }
                                | SubagentTerminal::Cancelled(_),
                            )
                            | OperationStatus::ExternallyAmbiguous(_) => {}
                            OperationStatus::Admitted | OperationStatus::Running(_) => {
                                return Err(SubagentError::Corruption {
                                    operation: operation.clone(),
                                    reason:
                                        "a delivering operation is neither terminal nor ambiguous"
                                            .into(),
                                });
                            }
                        }
                        let settlement = Inbox::admit_subagent_operation_delivery_within(
                            &tx,
                            &held,
                            event_id,
                            (operation, claim_id),
                            (limits.max_pending(), limits.max_projection_bytes()),
                            created_ms,
                        )?;
                        let n = tx.execute(
                            "UPDATE subagent_operation
                             SET delivery = 'delivered', delivery_settlement = ?4,
                                 time_delivered = ?2, time_updated = ?2
                             WHERE operation_id = ?1 AND delivery = 'delivering' AND claim_id = ?3",
                            rusqlite::params![
                                operation.as_str(),
                                created_ms,
                                claim_id.as_str(),
                                settlement.as_str(),
                            ],
                        )?;
                        Self::exactly_one(operation, n)?;
                        CasOutcome::Applied
                    }
                }
            }
        };
        tx.commit()?.discard();
        Ok(outcome)
    }

    /// Acknowledge one exact delivered fact and release its parent-inbox
    /// capacity. Delivery remains auditable on the operation row.
    pub fn acknowledge(
        &mut self,
        operation: &SubagentOperationId,
        claim_id: &DeliveryClaimId,
        generation: RuntimeGeneration,
        acknowledged_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        Self::acknowledge_on(
            &mut self.store,
            operation,
            claim_id,
            generation,
            acknowledged_ms,
        )
    }

    /// The same transition against the application's existing store owner.
    pub fn acknowledge_on(
        store: &mut EventStore,
        operation: &SubagentOperationId,
        claim_id: &DeliveryClaimId,
        generation: RuntimeGeneration,
        acknowledged_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        let tx = store.governed_transaction()?;
        let outcome = match Self::decode_row(&tx, operation)? {
            Some(DecodedRow {
                delivery:
                    DecodedDelivery::Claimed {
                        id,
                        input,
                        generation: claimed_generation,
                        delivered: true,
                        acknowledged: false,
                        ..
                    },
                ..
            }) if id == *claim_id && claimed_generation == generation => {
                let removed = tx.execute(
                    "DELETE FROM session_operation_delivery_pending
                     WHERE id = ?1 AND claim_id = ?2 AND generation = ?3",
                    rusqlite::params![
                        input.as_str(),
                        claim_id.as_str(),
                        generation_to_i64(generation)?,
                    ],
                )?;
                Self::exactly_one(operation, removed)?;
                let updated = tx.execute(
                    "UPDATE subagent_operation SET time_acknowledged = ?2, time_updated = ?2
                     WHERE operation_id = ?1 AND claim_id = ?3 AND claim_generation = ?4
                       AND delivery = 'delivered' AND time_acknowledged IS NULL",
                    rusqlite::params![
                        operation.as_str(),
                        acknowledged_ms,
                        claim_id.as_str(),
                        generation_to_i64(generation)?,
                    ],
                )?;
                Self::exactly_one(operation, updated)?;
                CasOutcome::Applied
            }
            Some(DecodedRow {
                delivery:
                    DecodedDelivery::Claimed {
                        id,
                        generation: claimed_generation,
                        acknowledged: true,
                        ..
                    },
                ..
            }) if id == *claim_id && claimed_generation == generation => {
                CasOutcome::AlreadyTerminal
            }
            _ => CasOutcome::NotLive,
        };
        tx.commit()?.discard();
        Ok(outcome)
    }
}

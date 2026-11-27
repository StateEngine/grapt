//! Write-side subagent operation service over `grapt_store::EventStore`.
//!
//! `subagent_operation` is a durable, directly-written table (the oracle's
//! subagent state is not event-sourced — `waivers.json` corrected-expectation).
//! It is NOT a generic status with a nullable-field bag: CHECK constraints
//! encode the tagged state and delivery unions, so an impossible combination
//! (running+delivered, unclaimed+claim, terminal payload leftovers) cannot be
//! written, and every read strictly decodes and fails closed. Every transition
//! decides legality and writes inside ONE immediate transaction via conditional
//! SQL. The one durable EVENT the domain emits is a typed operation-delivery
//! fact on the PARENT, appended through the inbox seam in the same
//! transaction that marks the operation Delivered — and its payload is DERIVED
//! from the stored admission + terminal + claim, never caller-authored.

use crate::operation::{
    ArtifactBackedOutput, ClaimRoute, DeliveryClaim, DeliveryLimits, DeliverySettlement,
    DeliveryStatus, OperationStatus, SubagentAdmission, SubagentCancelReason, SubagentMode,
    SubagentTerminal,
};
use grapt_inbox::Inbox;
use grapt_store::EventStore;
use grapt_types::{
    AgentId, AmbiguityEvidence, ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition,
    CasOutcome, DeliveryClaimId, EffectiveGraphId, EventId, MessageId, ModelId, ModelVariantId,
    OperationId, ProviderId, RuntimeGeneration, SessionId, SubagentOperationId, ToolCallId,
};

mod codec;
mod delivery;
mod followup;
mod listing;
mod message;
mod reads;

pub use followup::{
    FollowupCancelReason, FollowupDecision, FollowupReceipt, FollowupRun, FollowupStart,
    FollowupState,
};
pub use listing::{ChildOperation, ChildOperationPage};
pub use message::{AgentMessageReceipt, AgentMessageRequest};
pub use reads::ChildOperationContext;

use codec::{generation_to_i64, status_label, TerminalColumns};

#[derive(Debug, thiserror::Error)]
pub enum SubagentError {
    #[error("agent followup: {reason}")]
    Followup { reason: String },
    #[error("agent message: {reason}")]
    AgentMessage { reason: String },
    #[error("agent mail presentation: {reason}")]
    MailPresentation { reason: String },
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Inbox(#[from] grapt_inbox::InboxError),
    #[error("operation {operation}: durable record corrupt: {reason}")]
    Corruption {
        operation: SubagentOperationId,
        reason: String,
    },
    #[error("stored operation id {raw:?} is corrupt: {reason}")]
    CorruptOperationId { raw: String, reason: String },
    #[error("operation {operation}: claim route {route} does not match the {mode} mode")]
    MisroutedClaim {
        operation: SubagentOperationId,
        route: &'static str,
        mode: &'static str,
    },
    #[error("operation {operation}: cannot deliver a {route} claim as an operation delivery")]
    WrongRouteForDelivery {
        operation: SubagentOperationId,
        route: &'static str,
    },
    #[error(
        "operation {operation}: claim parent does not match the operation's immutable parent route"
    )]
    ClaimParentMismatch { operation: SubagentOperationId },
    #[error("operation {operation}: claim generation {presented} is stale; terminal generation is {terminal}")]
    StaleGeneration {
        operation: SubagentOperationId,
        presented: u64,
        terminal: u64,
    },
    #[error("operation {operation}: only a verified completed result can be delivered")]
    MissingDeliveryArtifact { operation: SubagentOperationId },
    #[error("runtime generation {value} does not fit the storage edge")]
    GenerationOutOfRange { value: u64 },
    #[error("artifact length {value} does not fit the storage edge")]
    ArtifactLengthOutOfRange { value: u64 },
    #[error("session {child} is recorded as the child of {count} subagent operations")]
    AmbiguousChild { child: String, count: usize },
}

/// Outcome of a delivery claim — the exactly-once arbiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This claim took a settled, undelivered operation.
    Won,
    /// Another claim already holds it, it is not settled, or it is unknown.
    Lost,
}

/// The durable subagent operation service.
#[derive(Debug)]
pub struct SubagentService {
    store: EventStore,
    limits: DeliveryLimits,
}

/// A fully-decoded delivery state. Every identity is a validated newtype, so a
/// corrupt persisted target cannot be trusted downstream — decode fails closed.
enum DecodedDelivery {
    Unclaimed,
    Claimed {
        id: DeliveryClaimId,
        route: ClaimRoute,
        parent: SessionId,
        input: MessageId,
        generation: RuntimeGeneration,
        delivered: bool,
        settlement: Option<DeliverySettlement>,
        acknowledged: bool,
        cleaned: bool,
    },
}

/// A strictly-decoded operation row. The immutable admission identities (child,
/// agent, parent route) and the delivery coordinates are validated newtypes;
/// any malformed persisted value is `Corruption`, so `get`/`reopen`/`deliver`
/// all fail closed rather than act on raw strings.
struct DecodedRow {
    subagent: SubagentOperationId,
    child: SessionId,
    parent: SessionId,
    mode: SubagentMode,
    run_generation: Option<RuntimeGeneration>,
    status: OperationStatus,
    delivery: DecodedDelivery,
}

const START_OPERATION_SQL: &str = "UPDATE subagent_operation
    SET status='running',generation=?2,run_generation=?2,time_updated=?3
    WHERE operation_id=?1 AND status='admitted'";

const CREATE_SUBAGENT_OPERATION: &str = "CREATE TABLE IF NOT EXISTS subagent_operation (
    operation_id TEXT PRIMARY KEY,
    family_operation_id TEXT NOT NULL UNIQUE,
    child_session_id TEXT NOT NULL,
    parent_session_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    model_provider TEXT,
    model_id TEXT,
    model_variant TEXT,
    description TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('background', 'foreground')),
    tool_call_id TEXT,
    effective_graph_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN
        ('admitted', 'running', 'completed', 'failed', 'cancelled', 'ambiguous')),
    generation INTEGER,
    run_generation INTEGER,
    output TEXT,
    artifact_id TEXT,
    artifact_length INTEGER,
    artifact_digest TEXT,
    artifact_disposition TEXT,
    error TEXT,
    reason TEXT CHECK (reason IS NULL OR reason IN ('process-loss', 'interrupted')),
    evidence TEXT,
    delivery TEXT NOT NULL CHECK (delivery IN ('unclaimed', 'delivering', 'delivered')),
    delivery_settlement TEXT CHECK (delivery_settlement IS NULL OR delivery_settlement IN
        ('completed', 'failed', 'quarantined')),
    claim_id TEXT,
    claim_generation INTEGER,
    claim_route TEXT CHECK (claim_route IS NULL OR claim_route IN ('synthetic', 'tool-result')),
    target_parent TEXT,
    target_input TEXT,
    time_delivered INTEGER,
    time_acknowledged INTEGER,
    time_cleaned INTEGER,
    time_created INTEGER NOT NULL,
    time_updated INTEGER NOT NULL,
    CHECK ((status = 'running') = (generation IS NOT NULL)),
    CHECK ((status = 'completed') = (output IS NOT NULL)),
    CHECK ((status = 'completed') = (artifact_id IS NOT NULL)),
    CHECK ((artifact_id IS NULL) = (artifact_length IS NULL)),
    CHECK ((artifact_id IS NULL) = (artifact_digest IS NULL)),
    CHECK ((artifact_id IS NULL) = (artifact_disposition IS NULL)),
    CHECK ((status = 'failed') = (error IS NOT NULL)),
    CHECK ((status = 'cancelled') = (reason IS NOT NULL)),
    CHECK ((status = 'ambiguous') = (evidence IS NOT NULL)),
    CHECK ((delivery = 'unclaimed') = (claim_id IS NULL)),
    CHECK ((claim_id IS NULL) = (claim_route IS NULL)),
    CHECK ((claim_id IS NULL) = (claim_generation IS NULL)),
    CHECK ((claim_id IS NULL) = (target_parent IS NULL)),
    CHECK ((claim_id IS NULL) = (target_input IS NULL)),
    CHECK ((delivery = 'delivered') = (time_delivered IS NOT NULL)),
    CHECK ((delivery = 'delivered') = (delivery_settlement IS NOT NULL)),
    CHECK (delivery_settlement <> 'completed' OR status = 'completed'),
    CHECK (delivery_settlement <> 'failed' OR status IN ('failed', 'cancelled')),
    CHECK (delivery_settlement <> 'quarantined' OR status = 'ambiguous'),
    CHECK (time_acknowledged IS NULL OR delivery = 'delivered'),
    CHECK (time_cleaned IS NULL OR time_acknowledged IS NOT NULL),
    CHECK (delivery = 'unclaimed'
           OR status IN ('completed', 'failed', 'cancelled', 'ambiguous')),
    CHECK (target_parent IS NULL OR target_parent = parent_session_id),
    CHECK ((model_provider IS NULL) = (model_id IS NULL)),
    CHECK (model_variant IS NULL OR model_id IS NOT NULL)
);";

impl SubagentService {
    pub fn new(store: EventStore, limits: DeliveryLimits) -> Result<Self, SubagentError> {
        Self::ensure_schema_on(&store)?;
        Ok(Self { store, limits })
    }

    /// Install the subagent schema on a BORROWED store.
    ///
    /// Idempotent, and safe to call on a store the product already opened: the
    /// inbox seam shares this database, because delivery admits on the parent.
    pub fn ensure_schema_on(store: &EventStore) -> Result<(), SubagentError> {
        Inbox::ensure_tables(store.connection())?;
        Self::migrate_subagent_table(store.connection())?;
        store
            .connection()
            .execute_batch(CREATE_SUBAGENT_OPERATION)?;
        store
            .connection()
            .execute_batch(followup::CREATE_FOLLOWUP_TABLES)?;
        Ok(())
    }

    /// [`Self::ensure_schema_on`] inside a transaction the caller holds, so a
    /// composition can install several owners' schemas in one commit. A
    /// legacy table needs its own migration transaction and is refused here;
    /// upgrade it through [`Self::ensure_schema_on`].
    pub fn ensure_schema_within(tx: &rusqlite::Transaction<'_>) -> Result<(), SubagentError> {
        Inbox::ensure_tables(tx)?;
        Self::migrate_subagent_table(tx)?;
        tx.execute_batch(CREATE_SUBAGENT_OPERATION)?;
        tx.execute_batch(followup::CREATE_FOLLOWUP_TABLES)?;
        Ok(())
    }

    fn migrate_subagent_table(conn: &rusqlite::Connection) -> Result<(), SubagentError> {
        let active_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'subagent_operation'",
            [],
            |row| row.get(0),
        )?;
        if active_exists == 0 {
            return Ok(());
        }
        let family_column: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('subagent_operation')
             WHERE name = 'family_operation_id'",
            [],
            |row| row.get(0),
        )?;
        if family_column == 0 {
            let legacy_exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'subagent_operation_legacy_synthetic'",
                [],
                |row| row.get(0),
            )?;
            if legacy_exists != 0 {
                return Err(SubagentError::Sqlite(rusqlite::Error::InvalidQuery));
            }
            conn.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE subagent_operation
                 RENAME TO subagent_operation_legacy_synthetic;
                 COMMIT;",
            )?;
            return Ok(());
        }
        let settlement_column: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('subagent_operation')
             WHERE name = 'delivery_settlement'",
            [],
            |row| row.get(0),
        )?;
        if settlement_column != 0 {
            return Ok(());
        }

        conn.execute_batch("BEGIN IMMEDIATE")?;
        let migrated = (|| -> Result<(), rusqlite::Error> {
            conn.execute_batch(
                "ALTER TABLE subagent_operation
                 RENAME TO subagent_operation_pre_delivery_settlement;",
            )?;
            conn.execute_batch(CREATE_SUBAGENT_OPERATION)?;
            conn.execute_batch(
                "INSERT INTO subagent_operation
                 (operation_id, family_operation_id, child_session_id, parent_session_id,
                  agent, model_provider, model_id, model_variant, description, mode,
                  tool_call_id, effective_graph_id, status, generation, run_generation,
                  output, artifact_id, artifact_length, artifact_digest, artifact_disposition,
                  error, reason, evidence, delivery, delivery_settlement, claim_id,
                  claim_generation, claim_route, target_parent, target_input, time_delivered,
                  time_acknowledged, time_cleaned, time_created, time_updated)
                 SELECT operation_id, family_operation_id, child_session_id, parent_session_id,
                        agent, model_provider, model_id, model_variant, description, mode,
                        tool_call_id, effective_graph_id, status, generation, run_generation,
                        output, artifact_id, artifact_length, artifact_digest,
                        artifact_disposition, error, reason, evidence, delivery,
                        CASE
                          WHEN delivery = 'delivered' AND status = 'completed' THEN 'completed'
                          WHEN delivery = 'delivered' AND status IN ('failed', 'cancelled')
                            THEN 'failed'
                          ELSE NULL
                        END,
                        claim_id, claim_generation, claim_route, target_parent, target_input,
                        time_delivered, time_acknowledged, time_cleaned, time_created, time_updated
                 FROM subagent_operation_pre_delivery_settlement;
                 DROP TABLE subagent_operation_pre_delivery_settlement;",
            )?;
            Ok(())
        })();
        match migrated {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _rollback_result = conn.execute_batch("ROLLBACK");
                return Err(SubagentError::Sqlite(error));
            }
        }
        Ok(())
    }

    pub fn store(&self) -> &EventStore {
        &self.store
    }

    /// Admit an operation (not yet running). The admission facts are immutable;
    /// a duplicate operation id is refused by the primary key. The effective
    /// graph is a required admission fact.
    pub fn open(
        &mut self,
        admission: &SubagentAdmission,
        created_ms: i64,
    ) -> Result<(), SubagentError> {
        Self::open_on(&self.store, admission, created_ms)
    }

    /// Admit one subagent operation against a BORROWED store.
    pub fn open_on(
        store: &EventStore,
        admission: &SubagentAdmission,
        created_ms: i64,
    ) -> Result<(), SubagentError> {
        let admission_conn = store.connection();
        Self::open_within(admission_conn, admission, created_ms)
    }

    fn open_within(
        admission_conn: &rusqlite::Connection,
        admission: &SubagentAdmission,
        created_ms: i64,
    ) -> Result<(), SubagentError> {
        let (mp, mi, mv) = match &admission.model {
            Some(model) => (
                Some(model.provider().as_str().to_owned()),
                Some(model.model().as_str().to_owned()),
                model.variant().as_ref().map(|v| v.as_str().to_owned()),
            ),
            None => (None, None, None),
        };
        admission_conn.execute(
            "INSERT INTO subagent_operation
             (operation_id, family_operation_id, child_session_id, parent_session_id, agent,
              model_provider, model_id, model_variant, description, mode,
              tool_call_id, effective_graph_id, status, delivery,
              time_created, time_updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     'admitted', 'unclaimed', ?13, ?13)",
            rusqlite::params![
                admission.subagent.as_str(),
                admission.operation.as_str(),
                admission.child.as_str(),
                admission.parent.as_str(),
                admission.agent.as_str(),
                mp,
                mi,
                mv,
                admission.description,
                admission.mode.as_str(),
                admission.tool_call.as_ref().map(|t| t.as_str().to_owned()),
                admission.effective_graph.as_str(),
                created_ms,
            ],
        )?;
        Ok(())
    }

    /// Start an admitted operation running under a runtime generation.
    pub fn start(
        &mut self,
        operation: &SubagentOperationId,
        generation: RuntimeGeneration,
        updated_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        Self::start_on(&mut self.store, operation, generation, updated_ms)
    }

    /// Start an admitted operation against a BORROWED store.
    pub fn start_on(
        store: &mut EventStore,
        operation: &SubagentOperationId,
        generation: RuntimeGeneration,
        updated_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        let generation = generation_to_i64(generation)?;
        Self::cas_status_on(
            store,
            operation,
            START_OPERATION_SQL,
            rusqlite::params![operation.as_str(), generation, updated_ms],
            &["admitted"],
        )
    }

    /// Settle a running operation to a domain terminal. Delivery is untouched —
    /// terminal does NOT imply delivered.
    pub fn settle(
        &mut self,
        operation: &SubagentOperationId,
        terminal: &SubagentTerminal,
        updated_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        Self::settle_on(&mut self.store, operation, terminal, updated_ms)
    }

    /// Settle a running operation against a BORROWED store.
    ///
    /// Delivery is untouched here, exactly as in the owning form: a domain
    /// terminal does NOT imply delivered.
    pub fn settle_on(
        store: &mut EventStore,
        operation: &SubagentOperationId,
        terminal: &SubagentTerminal,
        updated_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        let status = terminal.status_str();
        let terminal = TerminalColumns::from_terminal(terminal)?;
        Self::cas_status_on(
            store,
            operation,
            "UPDATE subagent_operation
             SET status = ?2, output = ?3, error = ?4, reason = ?5,
                 artifact_id = ?6, artifact_length = ?7, artifact_digest = ?8,
                 artifact_disposition = ?9, generation = NULL, time_updated = ?10
             WHERE operation_id = ?1 AND status = 'running'",
            rusqlite::params![
                operation.as_str(),
                status,
                terminal.output,
                terminal.error,
                terminal.reason,
                terminal.artifact_id,
                terminal.artifact_length,
                terminal.artifact_digest,
                terminal.artifact_disposition,
                updated_ms
            ],
            &["running"],
        )
    }

    /// A running operation whose external effect is ambiguous (a dead runtime
    /// with a possibly-live provider/tool effect) becomes ExternallyAmbiguous —
    /// never auto-replayed; a later `reconcile` moves it to a terminal.
    pub fn mark_ambiguous(
        &mut self,
        operation: &SubagentOperationId,
        evidence: &AmbiguityEvidence,
        updated_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        self.cas_status(
            operation,
            "UPDATE subagent_operation
             SET status = 'ambiguous', evidence = ?2, generation = NULL, time_updated = ?3
             WHERE operation_id = ?1 AND status = 'running'",
            rusqlite::params![operation.as_str(), evidence.as_str(), updated_ms],
            &["running"],
        )
    }

    /// Reconcile an ExternallyAmbiguous operation to a domain terminal. This is
    /// the ONLY path out of ambiguity — there is no auto-replay.
    pub fn reconcile(
        &mut self,
        operation: &SubagentOperationId,
        terminal: &SubagentTerminal,
        updated_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        let status = terminal.status_str();
        let terminal = TerminalColumns::from_terminal(terminal)?;
        self.cas_status(
            operation,
            "UPDATE subagent_operation
             SET status = ?2, output = ?3, error = ?4, reason = ?5,
                 artifact_id = ?6, artifact_length = ?7, artifact_digest = ?8,
                 artifact_disposition = ?9, evidence = NULL, time_updated = ?10
             WHERE operation_id = ?1 AND status = 'ambiguous'",
            rusqlite::params![
                operation.as_str(),
                status,
                terminal.output,
                terminal.error,
                terminal.reason,
                terminal.artifact_id,
                terminal.artifact_length,
                terminal.artifact_digest,
                terminal.artifact_disposition,
                updated_ms
            ],
            &["ambiguous"],
        )
    }

    /// Cleanup is a separate terminal transition after acknowledgment. It
    /// never erases the operation, delivery claim, artifact reference or event.
    pub fn cleanup(
        &mut self,
        operation: &SubagentOperationId,
        claim_id: &DeliveryClaimId,
        generation: RuntimeGeneration,
        cleaned_ms: i64,
    ) -> Result<CasOutcome, SubagentError> {
        let tx = self.store.governed_transaction()?;
        let outcome = match Self::decode_row(&tx, operation)? {
            Some(DecodedRow {
                delivery:
                    DecodedDelivery::Claimed {
                        id,
                        generation: claimed_generation,
                        acknowledged: true,
                        cleaned: false,
                        ..
                    },
                ..
            }) if id == *claim_id && claimed_generation == generation => {
                let updated = tx.execute(
                    "UPDATE subagent_operation SET time_cleaned = ?2, time_updated = ?2
                     WHERE operation_id = ?1 AND claim_id = ?3 AND claim_generation = ?4
                       AND time_acknowledged IS NOT NULL AND time_cleaned IS NULL",
                    rusqlite::params![
                        operation.as_str(),
                        cleaned_ms,
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
                        cleaned: true,
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

    /// Reopen a lost/incomplete claim ONLY behind typed proof the ORIGINAL route
    /// recorded no delivery: the STORED target parent must carry no admitted
    /// event for the STORED target input, and the presented `claim_id` must be
    /// the one that holds the operation. Binds the same claim + parent + input.
    pub fn reopen_if_undelivered(
        &mut self,
        operation: &SubagentOperationId,
        claim_id: &DeliveryClaimId,
        updated_ms: i64,
    ) -> Result<bool, SubagentError> {
        let tx = self.store.governed_transaction()?;
        let reopened = match Self::decode_row(&tx, operation)? {
            Some(DecodedRow {
                delivery:
                    DecodedDelivery::Claimed {
                        id,
                        parent,
                        input,
                        delivered: false,
                        ..
                    },
                ..
            }) if id.as_str() == claim_id.as_str() => {
                // The proof query runs on DECODED, validated coordinates — a
                // corrupt persisted target would have failed decode above.
                let recorded: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM event
                     WHERE type = 'session.input.admitted.1'
                       AND aggregate_id = ?1
                       AND json_extract(data, '$.inputID') = ?2",
                    rusqlite::params![parent.as_str(), input.as_str()],
                    |r| r.get(0),
                )?;
                if recorded == 0 {
                    let n = tx.execute(
                        "UPDATE subagent_operation
                         SET delivery = 'unclaimed', claim_id = NULL, claim_generation = NULL,
                             claim_route = NULL,
                             target_parent = NULL, target_input = NULL, time_updated = ?2
                         WHERE operation_id = ?1 AND delivery = 'delivering' AND claim_id = ?3",
                        rusqlite::params![operation.as_str(), updated_ms, claim_id.as_str()],
                    )?;
                    n == 1
                } else {
                    false
                }
            }
            _ => false,
        };
        tx.commit()?.discard();
        Ok(reopened)
    }

    /// [`Self::settle_dead_generation`] against a BORROWED store.
    ///
    /// The owning form and this one exist for the same reason every other
    /// `*_on` pair does: this service owns an `EventStore`, while the product's
    /// one store is owned by the `Inbox` and only ever lent out.
    pub fn settle_dead_generation_on(
        store: &mut EventStore,
        current: RuntimeGeneration,
        updated_ms: i64,
    ) -> Result<Vec<SubagentOperationId>, SubagentError> {
        let current_gen = generation_to_i64(current)?;
        let pending: bool = store.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM subagent_operation
             WHERE status = 'running' AND generation IS NOT NULL AND generation <> ?1)",
            [current_gen],
            |row| row.get(0),
        )?;
        if !pending {
            return Ok(Vec::new());
        }
        // Re-read and fully decode under the write transaction before changing rows.
        let tx = store.governed_transaction()?;
        let raw: Vec<String> = tx
            .prepare(
                "SELECT operation_id FROM subagent_operation
                 WHERE status = 'running' AND generation IS NOT NULL AND generation <> ?1
                 ORDER BY time_created",
            )?
            .query_map([current_gen], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        let mut decoded = Vec::with_capacity(raw.len());
        for id in raw {
            let parsed =
                SubagentOperationId::new(id.clone()).ok_or(SubagentError::CorruptOperationId {
                    raw: id,
                    reason: "empty operation id".into(),
                })?;
            Self::decode_row(&tx, &parsed)?;
            decoded.push(parsed);
        }
        tx.execute(
            "UPDATE subagent_operation
             SET status = 'cancelled', reason = 'process-loss', generation = NULL, time_updated = ?2
             WHERE status = 'running' AND generation IS NOT NULL AND generation <> ?1",
            rusqlite::params![current_gen, updated_ms],
        )?;
        tx.commit()?.discard();
        Ok(decoded)
    }

    /// Recover after a runtime replacement: every operation still `running`
    /// under a generation OTHER than `current` settles `Cancelled(ProcessLoss)`
    /// — never replayed. All selected ids are strictly decoded BEFORE any
    /// mutation, so a corrupt id fails closed without a partial write.
    pub fn settle_dead_generation(
        &mut self,
        current: RuntimeGeneration,
        updated_ms: i64,
    ) -> Result<Vec<SubagentOperationId>, SubagentError> {
        Self::settle_dead_generation_on(&mut self.store, current, updated_ms)
    }

    fn cas_status(
        &mut self,
        operation: &SubagentOperationId,
        sql: &str,
        params: impl rusqlite::Params,
        from_states: &[&str],
    ) -> Result<CasOutcome, SubagentError> {
        Self::cas_status_on(&mut self.store, operation, sql, params, from_states)
    }

    /// The status CAS against a BORROWED store.
    ///
    /// The owning form above delegates here. Both exist because the two callers
    /// hold the store differently and neither can be made to hold it the other
    /// way: this service owns an `EventStore`, while the product's one store is
    /// owned by the `Inbox` and only ever lent out. `FormService` already
    /// carries this exact pair — an owned service plus `*_on` statics — and it
    /// is why forms have a production caller and subagent operations did not.
    fn cas_status_on(
        store: &mut EventStore,
        operation: &SubagentOperationId,
        sql: &str,
        params: impl rusqlite::Params,
        from_states: &[&str],
    ) -> Result<CasOutcome, SubagentError> {
        let tx = store.governed_transaction()?;
        // Full strict decode BEFORE any write: a constraint-compatible corrupt
        // identity fails closed with zero writes, never a blind status CAS.
        let outcome = match Self::decode_row(&tx, operation)? {
            None => CasOutcome::NotLive,
            Some(row) if from_states.contains(&status_label(&row.status)) => {
                let n = tx.execute(sql, params)?;
                Self::exactly_one(operation, n)?;
                CasOutcome::Applied
            }
            Some(_) => CasOutcome::AlreadyTerminal,
        };
        tx.commit()?.discard();
        Ok(outcome)
    }
}

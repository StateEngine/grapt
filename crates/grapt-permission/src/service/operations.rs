use super::*;

impl PermissionService {
    pub fn new(store: EventStore) -> Result<Self, PermissionError> {
        store.connection().execute_batch(
            "CREATE TABLE IF NOT EXISTS permission_request (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                action TEXT NOT NULL,
                status TEXT NOT NULL,
                suspended_reason TEXT,
                reply TEXT
            );
            -- `Always` writes a rule scoped by the typed PROJECT supplied at the
            -- grant_always authority boundary — project is NOT NULL here and is
            -- never persisted on the asked event or request row.
            CREATE TABLE IF NOT EXISTS permission_rule (
                project_id TEXT NOT NULL,
                action TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                PRIMARY KEY (project_id, action)
            );",
        )?;
        Ok(Self {
            store,
            session_approvals: Vec::new(),
            waiters: Vec::new(),
        })
    }

    pub fn store(&self) -> &EventStore {
        &self.store
    }

    // ---- waiter (process-local projection) --------------------------------

    pub fn waiter(&self, id: &PermissionId) -> Waiter {
        self.waiters
            .iter()
            .rev()
            .find(|(w, _)| w == id)
            .map(|(_, g)| Waiter::Live(*g))
            .unwrap_or(Waiter::None)
    }

    /// Attach a process-local live waiter. Refuses unknown/suspended/terminal
    /// requests; only a live pending request may gain a waiter.
    pub fn attach_waiter(
        &mut self,
        id: &PermissionId,
        generation: RuntimeGeneration,
    ) -> Result<(), PermissionError> {
        // Read-only: attaching a waiter is process-local state, so this
        // transaction appends nothing and needs no fencing token.
        let tx = self.store.governed_transaction()?;
        let status = Self::read_row(&tx, id)?.map(|r| r.status);
        tx.commit()?.discard();
        match status {
            Some(Status::Pending) => {
                self.set_waiter(id, generation);
                Ok(())
            }
            _ => Err(PermissionError::NotLiveWaiter { id: id.clone() }),
        }
    }

    fn set_waiter(&mut self, id: &PermissionId, generation: RuntimeGeneration) {
        self.clear_waiter(id);
        self.waiters.push((id.clone(), generation));
    }

    fn clear_waiter(&mut self, id: &PermissionId) {
        self.waiters.retain(|(w, _)| w != id);
    }

    // ---- policy queries (grant material never leaves the owner) -----------

    /// Record that a Session reply covered an action, once.
    ///
    /// This is bookkeeping, not authority. Re-approving the same action in the
    /// same Session records nothing new, so the list cannot grow without bound
    /// across a long Session and cannot report one approval as several.
    fn record_session_approval(&mut self, session: SessionId, action: UnresolvedAction) {
        if self.is_recorded(&session, action.as_str()) {
            return;
        }
        self.session_approvals
            .push((session, UnresolvedApproval::record(action)));
    }

    /// Whether this Session's approval for `action` is already recorded.
    ///
    /// PRIVATE, and it must stay private. This used to be a public
    /// `allows_session(&self, session, action) -> bool`, which answered an
    /// AUTHORITY question — "may this Session do X?" — keyed on a raw action
    /// STRING. That is the one thing the rest of this crate is careful never to
    /// treat as resolved authority: a permission row names an action string and
    /// knows nothing about program, argv, cwd, environment, filesystem roots,
    /// egress or realized enforcement. A caller reaching for it in place of a
    /// real capability check would have been reading an unresolved action as
    /// resolved authority, and it invented policy semantics that belong to the
    /// later tool/policy resolution owner.
    ///
    /// As a private dedupe predicate it decides nothing about permission; it
    /// only stops the same fact being written twice. The observable difference
    /// between `Session` and `Always` remains durability, which
    /// [`PermissionService::has_saved_rule`] reports.
    fn is_recorded(&self, session: &SessionId, action: &str) -> bool {
        self.session_approvals
            .iter()
            .any(|(s, approval)| s == session && approval.action().as_str() == action)
    }

    /// Whether a durable `Always` project-rule exists for (project, action).
    pub fn has_saved_rule(
        &self,
        project: &ProjectId,
        action: &str,
    ) -> Result<bool, PermissionError> {
        let n: i64 = self.store.connection().query_row(
            "SELECT COUNT(*) FROM permission_rule WHERE project_id = ?1 AND action = ?2",
            rusqlite::params![project.as_str(), action],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    // ---- admission --------------------------------------------------------

    /// Issue a request with NO live waiter (create-without-waiter). The action
    /// is validated as non-empty up front so no later settlement can fail on
    /// it. It is deliberately NOT resolved into a capability here.
    pub fn create(
        &mut self,
        event_id: &EventId,
        id: &PermissionId,
        session: &SessionId,
        request: &PermissionRequest<'_>,
        created_ms: i64,
    ) -> Result<(), PermissionError> {
        self.admit(event_id, id, session, request, created_ms)
    }

    /// Issue a request AND structurally attach the live waiter (ask-and-await):
    /// the durable admission commits first, and only then is the waiter
    /// attached, so a failed admission leaves no waiter.
    pub fn ask(
        &mut self,
        event_id: &EventId,
        id: &PermissionId,
        session: &SessionId,
        request: &PermissionRequest<'_>,
        generation: RuntimeGeneration,
        created_ms: i64,
    ) -> Result<(), PermissionError> {
        self.admit(event_id, id, session, request, created_ms)?;
        self.set_waiter(id, generation);
        Ok(())
    }

    fn admit(
        &mut self,
        event_id: &EventId,
        id: &PermissionId,
        session: &SessionId,
        request: &PermissionRequest<'_>,
        created_ms: i64,
    ) -> Result<(), PermissionError> {
        if UnresolvedAction::new(request.action).is_none() {
            return Err(PermissionError::InvalidAction {
                action: request.action.to_owned(),
            });
        }
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        let data = json!({
            "id": id.as_str(),
            "sessionID": session.as_str(),
            "action": request.action,
            "resources": request.resources,
            "metadata": { "authority": { "layer": authority_layer(request.authority) } },
        });
        Self::append(
            &tx,
            &held,
            session,
            event_id,
            "permission.asked",
            &data,
            created_ms,
        )?;
        tx.commit()?.discard();
        Ok(())
    }

    // ---- suspension / cancel ----------------------------------------------

    pub fn suspend(
        &mut self,
        event_id: &EventId,
        id: &PermissionId,
        reason: PermissionSuspensionReason,
        created_ms: i64,
    ) -> Result<CasOutcome, PermissionError> {
        let outcome = self.settle_single(
            id,
            "permission.suspended",
            event_id,
            created_ms,
            |session, id| {
                json!({
                    "sessionID": session.as_str(),
                    "requestID": id.as_str(),
                    "reason": reason_str(reason),
                })
            },
            /*allow_suspended=*/ false,
        )?;
        if outcome == CasOutcome::Applied {
            self.clear_waiter(id);
        }
        Ok(outcome)
    }

    pub fn cancel(
        &mut self,
        event_id: &EventId,
        id: &PermissionId,
        created_ms: i64,
    ) -> Result<CasOutcome, PermissionError> {
        let outcome = Self::cancel_in_store(&mut self.store, event_id, id, created_ms)?;
        if outcome == CasOutcome::Applied {
            self.clear_waiter(id);
        }
        Ok(outcome)
    }

    pub(crate) fn cancel_in_store(
        store: &mut EventStore,
        event_id: &EventId,
        id: &PermissionId,
        created_ms: i64,
    ) -> Result<CasOutcome, PermissionError> {
        Self::cancel_in_store_because(
            store,
            event_id,
            id,
            created_ms,
            crate::CancelReason::Interrupted,
        )
    }

    pub(crate) fn cancel_in_store_because(
        store: &mut EventStore,
        event_id: &EventId,
        id: &PermissionId,
        created_ms: i64,
        reason: crate::CancelReason,
    ) -> Result<CasOutcome, PermissionError> {
        Self::settle_single_in_store(
            store,
            id,
            "permission.cancelled",
            event_id,
            created_ms,
            |session, id| json!({"sessionID":session.as_str(),"requestID":id.as_str(),"reason":reason.as_str()}),
            true,
        )
    }

    /// Shared single-transition helper for suspend/cancel. `allow_suspended`
    /// governs whether a suspended row is a legal source (cancel yes, suspend
    /// no — a suspended row cannot re-suspend).
    fn settle_single(
        &mut self,
        id: &PermissionId,
        kind: &str,
        event_id: &EventId,
        created_ms: i64,
        data_for: impl FnOnce(&SessionId, &PermissionId) -> Value,
        allow_suspended: bool,
    ) -> Result<CasOutcome, PermissionError> {
        Self::settle_single_in_store(
            &mut self.store,
            id,
            kind,
            event_id,
            created_ms,
            data_for,
            allow_suspended,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn settle_single_in_store(
        store: &mut EventStore,
        id: &PermissionId,
        kind: &str,
        event_id: &EventId,
        created_ms: i64,
        data_for: impl FnOnce(&SessionId, &PermissionId) -> Value,
        allow_suspended: bool,
    ) -> Result<CasOutcome, PermissionError> {
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        let outcome = match Self::read_row(&tx, id)? {
            None => CasOutcome::NotLive,
            Some(row) if row.status.is_terminal() => CasOutcome::AlreadyTerminal,
            Some(row) if matches!(row.status, Status::Suspended(_)) && !allow_suspended => {
                CasOutcome::AlreadyTerminal
            }
            Some(row) => {
                let session = Self::session_of(id, &row)?;
                let data = data_for(&session, id);
                Self::append(&tx, &held, &session, event_id, kind, &data, created_ms)?;
                CasOutcome::Applied
            }
        };
        tx.commit()?.discard();
        Ok(outcome)
    }

    // ---- settlement authority paths (distinct, never one generic branch) --

    /// Ordinary reply: `Once` or `Reject`. A direct reply to a suspended
    /// request is `NotLive` (zero writes); an explicit `Reject` on a live
    /// request cascades over the session's suspended siblings.
    pub fn reply(
        &mut self,
        id: &PermissionId,
        reply: OrdinaryReply,
        mut mint: impl FnMut() -> EventId,
        created_ms: i64,
    ) -> Result<ReplyOutcome, PermissionError> {
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        let outcome = match Self::read_row(&tx, id)? {
            None => ReplyOutcome {
                primary: CasOutcome::NotLive,
                cascaded: Vec::new(),
            },
            Some(row) if row.status.is_terminal() => ReplyOutcome {
                primary: CasOutcome::AlreadyTerminal,
                cascaded: Vec::new(),
            },
            Some(row) if matches!(row.status, Status::Suspended(_)) => ReplyOutcome {
                primary: CasOutcome::NotLive,
                cascaded: Vec::new(),
            },
            Some(row) => {
                let session = Self::session_of(id, &row)?;
                Self::append_reply(
                    &tx,
                    &held,
                    &session,
                    id,
                    reply.as_permission_reply(),
                    &mut mint,
                    created_ms,
                )?;
                let cascaded = if reply == OrdinaryReply::Reject {
                    Self::cascade_reject(&tx, &held, &session, id, &mut mint, created_ms)?
                } else {
                    Vec::new()
                };
                ReplyOutcome {
                    primary: CasOutcome::Applied,
                    cascaded,
                }
            }
        };
        tx.commit()?.discard();
        if outcome.primary == CasOutcome::Applied {
            self.clear_waiter(id);
        }
        Ok(outcome)
    }

    /// `Session` authority: settle with a process-local grant. The capability
    /// is validated BEFORE commit; the private grant is minted only after a
    /// successful durable commit and never persisted.
    pub fn grant_session(
        &mut self,
        id: &PermissionId,
        event_id: &EventId,
        created_ms: i64,
    ) -> Result<CasOutcome, PermissionError> {
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        let decided = match Self::read_row(&tx, id)? {
            None => Err(CasOutcome::NotLive),
            Some(row) if row.status.is_terminal() => Err(CasOutcome::AlreadyTerminal),
            Some(row) if matches!(row.status, Status::Suspended(_)) => Err(CasOutcome::NotLive),
            Some(row) => {
                let session = Self::session_of(id, &row)?;
                let action = UnresolvedAction::new(&row.action).ok_or_else(|| {
                    PermissionError::InvalidAction {
                        action: row.action.clone(),
                    }
                })?;
                Self::append_reply_kind(&tx, &held, &session, id, "session", event_id, created_ms)?;
                Ok((session, action))
            }
        };
        match decided {
            Err(outcome) => {
                tx.commit()?.discard();
                Ok(outcome)
            }
            Ok((session, action)) => {
                tx.commit()?.discard();
                self.clear_waiter(id);
                self.record_session_approval(session, action);
                Ok(CasOutcome::Applied)
            }
        }
    }

    /// `Always` authority: a durable, project-scoped rule. REFUSES with zero
    /// writes when no project resolves (never degrading to Once). When a
    /// project is supplied, the replied event AND the NOT-NULL project rule
    /// commit in ONE transaction.
    pub fn grant_always(
        &mut self,
        id: &PermissionId,
        project: Option<&ProjectId>,
        event_id: &EventId,
        created_ms: i64,
    ) -> Result<CasOutcome, PermissionError> {
        // No project → refuse before any read/write.
        let Some(project) = project else {
            return Ok(CasOutcome::NotLive);
        };
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        let outcome = match Self::read_row(&tx, id)? {
            None => CasOutcome::NotLive,
            Some(row) if row.status.is_terminal() => CasOutcome::AlreadyTerminal,
            Some(row) if matches!(row.status, Status::Suspended(_)) => CasOutcome::NotLive,
            Some(row) => {
                let session = Self::session_of(id, &row)?;
                Self::append_reply_kind(&tx, &held, &session, id, "always", event_id, created_ms)?;
                tx.execute(
                    "INSERT INTO permission_rule (project_id, action, time_created)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(project_id, action) DO NOTHING",
                    rusqlite::params![project.as_str(), row.action, created_ms],
                )?;
                CasOutcome::Applied
            }
        };
        tx.commit()?.discard();
        if outcome == CasOutcome::Applied {
            self.clear_waiter(id);
        }
        Ok(outcome)
    }

    fn append_reply(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        session: &SessionId,
        id: &PermissionId,
        reply: PermissionReply,
        mint: &mut impl FnMut() -> EventId,
        created_ms: i64,
    ) -> Result<(), PermissionError> {
        let data = json!({
            "sessionID": session.as_str(),
            "requestID": id.as_str(),
            "reply": crate::event::reply_as_str(reply),
        });
        let event = mint();
        Self::append(
            tx,
            held,
            session,
            &event,
            "permission.replied",
            &data,
            created_ms,
        )
    }

    fn append_reply_kind(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        session: &SessionId,
        id: &PermissionId,
        reply: &str,
        event_id: &EventId,
        created_ms: i64,
    ) -> Result<(), PermissionError> {
        let data = json!({
            "sessionID": session.as_str(),
            "requestID": id.as_str(),
            "reply": reply,
        });
        Self::append(
            tx,
            held,
            session,
            event_id,
            "permission.replied",
            &data,
            created_ms,
        )
    }

    fn cascade_reject(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        session: &SessionId,
        primary: &PermissionId,
        mint: &mut impl FnMut() -> EventId,
        created_ms: i64,
    ) -> Result<Vec<CascadeSettled>, PermissionError> {
        let siblings: Vec<(String, Option<String>)> = tx
            .prepare(
                "SELECT id, suspended_reason FROM permission_request
                 WHERE session_id = ?1 AND status = 'suspended' AND id != ?2",
            )?
            .query_map(rusqlite::params![session.as_str(), primary.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut settled = Vec::new();
        for (sibling_raw, reason_raw) in siblings {
            let sibling: PermissionId =
                sibling_raw.parse().map_err(|e: grapt_types::IdError| {
                    PermissionError::Corruption {
                        id: primary.clone(),
                        reason: format!("sibling id {sibling_raw:?}: {e}"),
                    }
                })?;
            let had = reason_raw
                .as_deref()
                .and_then(reason_from_str)
                .ok_or_else(|| PermissionError::Corruption {
                    id: sibling.clone(),
                    reason: "suspended sibling without a valid reason".into(),
                })?;
            Self::append_reply(
                tx,
                held,
                session,
                &sibling,
                PermissionReply::Reject,
                mint,
                created_ms,
            )?;
            settled.push(CascadeSettled {
                request: sibling,
                had_suspension: had,
            });
        }
        Ok(settled)
    }

    // ---- shared helpers ---------------------------------------------------

    fn append(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        session: &SessionId,
        event_id: &EventId,
        kind: &str,
        data: &Value,
        created_ms: i64,
    ) -> Result<(), PermissionError> {
        let version = EventVersion::new(1).ok_or(PermissionError::Version)?;
        // The writer epoch never enters this service's public signatures: the
        // token comes from the store this service already owns, snapshotted
        // before the transaction because the session is only known after the
        // row read inside it.
        let aggregate = AggregateId::Session(session.clone());
        let writer = held.token(&aggregate)?;
        EventStore::append_within(
            tx,
            AppendInput {
                id: event_id,
                aggregate,
                writer,
                kind_name: kind,
                version,
                created_ms,
                data,
            },
            &mut [&mut PermissionProjector],
        )?;
        Ok(())
    }

    fn session_of(id: &PermissionId, row: &Row) -> Result<SessionId, PermissionError> {
        row.session
            .parse()
            .map_err(|e: grapt_types::IdError| PermissionError::Corruption {
                id: id.clone(),
                reason: e.to_string(),
            })
    }

    pub(super) fn read_row(
        tx: &Transaction<'_>,
        id: &PermissionId,
    ) -> Result<Option<Row>, PermissionError> {
        type RawRow = (String, String, String, Option<String>);
        let raw: Option<RawRow> = tx
            .query_row(
                "SELECT session_id, action, status, suspended_reason
                 FROM permission_request WHERE id = ?1",
                [id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        let Some((session, action, status_raw, reason_raw)) = raw else {
            return Ok(None);
        };
        let corrupt = |reason: String| PermissionError::Corruption {
            id: id.clone(),
            reason,
        };
        let suspended_reason = match reason_raw {
            Some(r) => Some(reason_from_str(&r).ok_or_else(|| {
                corrupt(format!("suspended_reason {r:?} outside the vocabulary"))
            })?),
            None => None,
        };
        let status = match (status_raw.as_str(), suspended_reason) {
            ("pending", _) => Status::Pending,
            ("replied", _) => Status::Replied,
            ("suspended", Some(reason)) => Status::Suspended(reason),
            ("suspended", None) => return Err(corrupt("suspended row without a reason".into())),
            ("cancelled", _) => Status::Cancelled,
            (other, _) => {
                return Err(corrupt(format!(
                    "status {other:?} outside the closed vocabulary"
                )));
            }
        };
        Ok(Some(Row {
            session,
            action,
            status,
        }))
    }
}

//! What the producer has been MADE to hold: sessions, turns, asks and forms.
//!
//! The other half of the double. `double.rs` owns its state, bounds and
//! transport seams; this owns the acts a witness performs on it — create a
//! session, run a turn, raise an ask, raise a form. Split by subject when the
//! file passed this workspace's module ceiling.

use super::*;

impl ContractDouble {
    /// Raise one pending form on a session, as the product does mid-run.
    ///
    /// Returns whether it was raised: an unknown session is refused rather
    /// than producing a form nothing can answer, and the per-session bound is
    /// the same one the permission asks use, applied at the producer's own
    /// ownership point.
    pub fn raise_form(
        &mut self,
        session: &SessionWireId,
        title: &str,
    ) -> Option<crate::wire::FormWireId> {
        if !self.sessions.contains_key(session) {
            return None;
        }
        let owned = self
            .forms
            .iter()
            .filter(|form| form.row.owner.session() == Some(session))
            .count();
        if owned >= self.bounds.max_retained_permissions {
            return None;
        }
        self.next_id = self.next_id.saturating_add(1);
        let id = crate::wire::form_id(&format!("frm_{:012x}", self.next_id)).ok()?;
        let row = crate::wire::FormRow {
            metadata: Default::default(),
            id: id.clone(),
            owner: crate::wire::FormOwner::Session(session.clone()),
            title: title.to_owned(),
            fields: vec![
                crate::wire::FormField {
                    attributes: Default::default(),
                    key: "branch".to_owned(),
                    kind: "string".to_owned(),
                    title: Some("Branch to work on".to_owned()),
                    required: true,
                },
                crate::wire::FormField {
                    attributes: Default::default(),
                    key: "dry_run".to_owned(),
                    kind: "boolean".to_owned(),
                    title: None,
                    required: false,
                },
            ],
        };
        self.forms.push(DoubleForm {
            row,
            state: crate::wire::FormState::Pending,
        });
        Some(id)
    }

    /// Suspend one raised form, as a shutdown or a lost process does.
    ///
    /// Suspension is NOT a settlement: the form stays listable and stays
    /// replyable. This exists so the client's suspended handling is driven by a
    /// producer transition rather than by a hand-built response — the only way
    /// to witness that answering a suspended form still settles it truthfully.
    ///
    /// Returns whether the form was suspended. A form that is already settled
    /// is not suspended: a shutdown does not reopen an answered question.
    pub fn suspend_form(
        &mut self,
        id: &crate::wire::FormWireId,
        reason: crate::wire::FormSuspension,
    ) -> bool {
        let Some(form) = self.forms.iter_mut().find(|form| &form.row.id == id) else {
            return false;
        };
        if !form.state.is_open() {
            return false;
        }
        form.state = crate::wire::FormState::Suspended(reason);
        true
    }

    /// The settlement state the producer holds for one form.
    pub fn form_state(&self, id: &crate::wire::FormWireId) -> Option<&crate::wire::FormState> {
        self.forms
            .iter()
            .find(|form| &form.row.id == id)
            .map(|form| &form.state)
    }

    /// Seed a session with a title, returning its id.
    ///
    /// Returns `None` when the session-retention bound is already at its limit.
    /// A producer that silently accepted an unbounded number of sessions is
    /// exactly the defect review named at this ownership point, and a producer that
    /// silently DROPPED one would be no better — so the refusal is in the return
    /// type, and it is counted.
    pub fn create_session(&mut self, title: &str) -> Option<SessionWireId> {
        // A REAL domain identity: `ses_` plus twelve hex. The previous
        // revision minted `ses1`, which no `SessionId` would ever have parsed.
        self.next_id = self.next_id.saturating_add(1);
        let spelled = format!("ses_{:012x}", self.next_id);
        // Parsed, not trusted. The double is held to the same identity law as
        // the client: if this mint ever drifted to a shape `SessionId` refuses,
        // the double would fail here rather than serving an identity the client
        // must then refuse at the edge.
        let id = crate::wire::session_id(&spelled).ok()?;
        if !self.sessions.contains_key(&id)
            && self.sessions.len() >= self.bounds.max_retained_sessions
        {
            self.dropped_sessions = self.dropped_sessions.saturating_add(1);
            return None;
        }
        self.sessions.insert(
            id.clone(),
            DoubleSession {
                project: None,
                rewind_receipts: Vec::new(),
                preset: None,
                messages: Vec::new(),
                log: Vec::new(),
                next_seq: 0,
                title: title.to_owned(),
                dropped_messages: 0,
                dropped_log_entries: 0,
                dropped_permissions: 0,
                agent: None,
                model: None,
                variant: None,
                parent: None,
                state: None,
            },
        );
        let data = serde_json::json!({"sessionID": spelled, "title": title});
        self.append(&id, EventMember::SessionCreated, data);
        if let Some(entry) = self.sessions.get_mut(&id) {
            // A new session reads idle at its creation, as the product's does.
            entry.state = Some(positions::ExecutionState::idle(entry.next_seq));
        }
        Some(id)
    }

    /// Create a session SPAWNED BY another one, the way a subagent's is.
    ///
    /// The parent is recorded so the session page serves a real `parentID`.
    /// Without this the double could only ever produce root sessions, and a
    /// client that dropped lineage entirely would look correct.
    pub fn create_child_session(
        &mut self,
        title: &str,
        parent: &SessionWireId,
    ) -> Option<SessionWireId> {
        let id = self.create_session(title)?;
        self.sessions.get_mut(&id)?.parent = Some(parent.clone());
        Some(id)
    }

    /// Append one durable fact and queue its frame.
    ///
    /// The durable log is BOUNDED here, at the producer's own ownership point.
    /// The previous revision pushed unconditionally, which is the shape review
    /// named: bounding a pump iteration bounds the CONSUMER while the
    /// producer's storage grows forever.
    ///
    /// The sequence counter is deliberately NOT rewound by an eviction. `seq` is
    /// durable position, not an index into retained memory; rewinding it on
    /// eviction would make two different facts share one sequence number and
    /// would silently corrupt every watermark the client holds. Evicting the
    /// oldest entry loses the client's ability to RE-READ that history, which is
    /// exactly what the truncated page then declares.
    pub(super) fn append(
        &mut self,
        session: &SessionWireId,
        member: EventMember,
        data: serde_json::Value,
    ) {
        let max = self.bounds.max_retained_log_entries;
        let Some(entry) = self.sessions.get_mut(session) else {
            return;
        };
        entry.next_seq = entry.next_seq.saturating_add(1);
        let seq = entry.next_seq;
        entry.log.push(DurableEntry {
            seq,
            member,
            data: data.clone(),
        });
        while entry.log.len() > max {
            entry.log.remove(0);
            entry.dropped_log_entries = entry.dropped_log_entries.saturating_add(1);
        }
        let frame = Self::render(member, &data, Some((session, seq)), self.next_id);
        self.queue(frame);
    }

    /// Queue one live (non-durable) frame.
    pub(super) fn live(&mut self, member: EventMember, data: serde_json::Value) {
        let frame = Self::render(member, &data, None, self.next_id);
        self.queue(frame);
    }

    /// Render one frame in the exact frozen envelope shape.
    ///
    /// `created` is present exactly when the member requires it and `durable`
    /// exactly when the member requires it — the double is held to the same
    /// asymmetry the client enforces, so a bug in one cannot mask a bug in the
    /// other.
    pub(super) fn render(
        member: EventMember,
        data: &serde_json::Value,
        durable: Option<(&SessionWireId, u64)>,
        counter: u64,
    ) -> String {
        let mut object = serde_json::Map::new();
        object.insert("id".into(), serde_json::json!(format!("evt_{counter}")));
        object.insert("type".into(), serde_json::json!(member.as_str()));
        if member.requires_created() {
            // A fixed literal, not a clock. Determinism is the point.
            object.insert("created".into(), serde_json::json!(1_700_000_000_000u64));
        }
        if member.requires_durable() {
            if let Some((aggregate, seq)) = durable {
                object.insert(
                    "durable".into(),
                    serde_json::json!({
                        "aggregateID": aggregate.spelled(),
                        "seq": seq,
                        "version": 1
                    }),
                );
            }
        }
        object.insert("data".into(), data.clone());
        serde_json::Value::Object(object).to_string()
    }

    /// Run one scripted assistant turn for a session.
    ///
    /// Deliberately exercises both content channels, usage, and a terminal, so
    /// a client proved against it has rendered every shape the essential
    /// journey contains.
    pub fn run_turn(&mut self, session: &SessionWireId, prompt: &str, reply: &str) {
        self.run_turn_as(session, prompt, reply, None);
    }

    /// [`Self::run_turn`] for a prompt that carried its own input id.
    pub fn run_turn_as(
        &mut self,
        session: &SessionWireId,
        prompt: &str,
        reply: &str,
        input: Option<String>,
    ) {
        // The durable record the re-list will serve: a user message and an
        // assistant message carrying ordered content items. These are the SAME
        // facts the live stream decorates, which is what lets the client's
        // convergence witness compare the two.
        //
        // The user message is the PROMPT that was actually submitted. It used
        // to be the literal `"hello"` whatever had been sent, so reopening a
        // session showed the user text they never typed — which reads exactly
        // like a client rendering the transcript wrongly, and which meant the
        // producer's durable truth disagreed with the request that produced it.
        self.record(session, DoubleMessage::User(prompt.to_owned(), input));
        self.record(
            session,
            DoubleMessage::Assistant(
                vec![
                    crate::wire::MessageItem::Reasoning("considering".to_owned()),
                    crate::wire::MessageItem::Text(reply.to_owned()),
                ],
                // A BLOCKED TURN HAS NOT FINISHED, durably either. Recording a
                // finish here while the live stream withheld its terminal made
                // the double disagree with itself: reconciling a turn that was
                // still waiting on authority settled it as SUCCEEDED. A
                // producer cannot both be waiting and have stopped.
                if self.fault == Fault::BlockedOnPermission {
                    None
                } else {
                    Some(crate::wire::DurableFinish::Stop)
                },
            ),
        );
        let spelled = session.spelled().to_owned();
        let execution = self.mint("exe");
        let data = serde_json::json!({"sessionID": spelled, "executionID": execution});
        self.append(session, EventMember::ExecutionStarted, data);
        self.observe_state(session, "running", Some(execution.clone()));

        // Every live block names its assistant row and ordinal, as the real
        // wire does: the row just recorded, reasoning first, then text.
        let message = format!(
            "msg_{:012x}",
            self.sessions
                .get(session)
                .map_or(0, |entry| entry.messages.len())
        );
        let block = |ordinal: u64| serde_json::json!({"sessionID": spelled, "assistantMessageID": message, "ordinal": ordinal});
        let chunk = |ordinal: u64, delta: &str| {
            let mut data = block(ordinal);
            data["delta"] = serde_json::json!(delta);
            data
        };
        self.append(session, EventMember::ReasoningStarted, block(0));
        self.delta(EventMember::ReasoningDelta, chunk(0, "considering"));
        // Every end carries its block's whole text, as the product's does.
        let ended = |ordinal: u64, text: &str| {
            let mut data = block(ordinal);
            data["text"] = serde_json::json!(text);
            data
        };
        self.append(
            session,
            EventMember::ReasoningEnded,
            ended(0, "considering"),
        );

        self.append(session, EventMember::TextStarted, block(1));
        for piece in reply.split_inclusive(' ') {
            self.delta(EventMember::TextDelta, chunk(1, piece));
        }
        self.append(session, EventMember::TextEnded, ended(1, reply));

        self.live(
            EventMember::UsageUpdated,
            serde_json::json!({"sessionID": spelled, "input": 12, "output": 7}),
        );
        if self.fault == Fault::BlockedOnPermission {
            // A FAILED CALL ALREADY IN DURABLE HISTORY, then a wait for
            // authority to try again. This is the observed shape: the first
            // shell command failed, and the model asked to run another.
            //
            // It is recorded durably because that is the only route a tool
            // reaches this client, and because the point of the witness is
            // whether the client SHOWS it before asking again.
            let failed = crate::wire::DurableTool {
                duration_ms: None,
                progress: Vec::new(),
                id: match crate::wire::tool_call_id("call_000000000077") {
                    Ok(id) => id,
                    Err(_) => return,
                },
                name: "shell".to_owned(),
                state: crate::wire::DurableToolState::Error {
                    input: "{\"command\":\"printf ALPHAMARKER > j03.txt\"}".to_owned(),
                    error: crate::wire::DurableError {
                        kind: "effect-failed".to_owned(),
                        message: "shell exited with exit code: 1".to_owned(),
                    },
                    output: None,
                },
            };
            self.record(
                session,
                DoubleMessage::Assistant(vec![crate::wire::MessageItem::Tool(failed)], None),
            );
            // Waiting on authority, so there is no terminal and no idle. That
            // is what the screen shows while a tool is blocked, and it is why
            // the execution stays `running`.
            self.blocked_session = Some(session.clone());
            self.reads_since_blocked = 0;
            return;
        }
        let data = serde_json::json!({"sessionID": spelled, "executionID": execution});
        self.append(session, EventMember::ExecutionSucceeded, data);
        // A positioned double reports the settled state with its revision on
        // the status frame; a legacy one keeps the bare idle status.
        // The quiescence edge the engine reports at settle, in the order the
        // engine emits it: the status frame first, then the retained
        // vocabulary frame. AFTER the terminal on purpose — a pump that
        // applies a terminal pauses later live frames until durable hydration,
        // so a quiescence edge before the terminal would reach the view in the
        // same pump and the terminal-hydration witnesses would see quiescence
        // they are written to prove is absent.
        if self.positions_enabled() {
            self.observe_state(session, "completed", Some(execution));
        } else {
            self.live(
                EventMember::SessionStatus,
                serde_json::json!({"sessionID": spelled, "status": {"type": "idle"}}),
            );
        }
        self.live(
            EventMember::SessionIdle,
            serde_json::json!({"sessionID": spelled}),
        );
    }

    /// How many times the outstanding-ask list has been read.
    pub fn permission_list_calls(&self) -> usize {
        self.permission_list_calls
    }

    /// The sessions a compaction was admitted for, in order.
    pub fn compactions(&self) -> &[SessionWireId] {
        &self.compactions
    }

    /// Raise a permission ask against a session.
    ///
    /// Returns `None` when the per-session ask bound refuses it. Unbounded here
    /// was two unbounded dimensions in one field: an unbounded map of unbounded
    /// vectors, neither of which any client bound could reach.
    pub fn ask_permission(&mut self, session: &SessionWireId, title: &str) -> Option<String> {
        self.ask_permission_previewed(session, title, None)
    }

    /// The same ask, carrying the display preview the engine attaches.
    pub fn ask_permission_previewed(
        &mut self,
        session: &SessionWireId,
        title: &str,
        preview: Option<serde_json::Value>,
    ) -> Option<String> {
        let max = self.bounds.max_retained_permissions;
        let request_id = self.mint("per_");
        let asks = self.permissions.entry(session.clone()).or_default();
        if asks.len() >= max {
            if let Some(entry) = self.sessions.get_mut(session) {
                entry.dropped_permissions = entry.dropped_permissions.saturating_add(1);
            }
            return None;
        }
        asks.push(PendingPermission {
            request_id: request_id.clone(),
            title: title.to_owned(),
            settled: None,
            preview,
        });
        Some(request_id)
    }

    /// Drop every ask a session has open, as cancelling the tools that raised
    /// them does. The client is not told; it learns by re-reading.
    pub fn withdraw_permissions(&mut self, session: &SessionWireId) {
        self.permissions.remove(session);
    }

    /// The outstanding asks for a session.
    pub fn permissions(&self, session: &SessionWireId) -> &[PendingPermission] {
        self.permissions
            .get(session)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

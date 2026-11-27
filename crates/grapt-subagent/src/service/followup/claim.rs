//! One atomic transfer from a queued native wake to a freshly claimed run.
use super::*;
use grapt_inbox::{AdmitOutcome, PendingInput, SyntheticAdmitRequest};
use grapt_types::DeliveryMode;

fn validate(request: &FollowupStart<'_>) -> Result<(), SubagentError> {
    let admission = request.admission;
    if request.created_ms < 0
        || request.generation.value() == 0
        || request.message.as_str().len() > 512
        || request.execution.as_str().len() > 512
        || request.input.as_str().len() > 512
        || request.input_event.as_str().len() > 512
        || admission.subagent.as_str().len() > 512
        || admission.operation.as_str().len() > 512
        || admission.effective_graph.as_str().len() > 512
        || admission.agent.as_str().len() > 512
        || admission.child.as_str().len() > 512
        || admission.parent.as_str().len() > 512
        || admission.description.len() > 4096
        || admission.mode != SubagentMode::Background
        || admission.tool_call.is_some()
    {
        return Err(refused(
            "followup run has invalid bounds, mode or caller-owned tool route",
        ));
    }
    if admission.model.as_ref().is_some_and(|model| {
        model.provider().as_str().len() > 512
            || model.model().as_str().len() > 512
            || model
                .variant()
                .as_ref()
                .is_some_and(|variant| variant.as_str().len() > 512)
    }) {
        return Err(refused("followup model identity exceeds its bound"));
    }
    Ok(())
}

impl SubagentService {
    /// The product must hold the child's existing Session wake custody before
    /// this call: operation terminality alone does not prove an executor stopped.
    /// No unclaimed successor is visible to ordinary prompt callers.
    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn claim_followup_on(
        store: &mut EventStore,
        request: &FollowupStart<'_>,
    ) -> Result<FollowupDecision, SubagentError> {
        validate(request)?;
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        let stored = intent::read(&tx, request.message)?
            .ok_or_else(|| refused("followup wake has no durable admission"))?;
        match stored.receipt.state {
            FollowupState::Started(_) => return Ok(FollowupDecision::AlreadyStarted),
            FollowupState::Consumed => return Ok(FollowupDecision::Consumed),
            FollowupState::Cancelled(reason) => return Ok(FollowupDecision::Cancelled(reason)),
            FollowupState::Queued => {}
        }
        if stored.generation != request.generation {
            return Err(refused("followup wake belongs to another runtime"));
        }
        super::super::message::authorize(&tx, &stored.sender, &stored.child)?;
        if intent::captured(&tx, &stored)? {
            tx.execute(
                "UPDATE subagent_followup SET state='consumed',time_updated=?2
                WHERE message_id=?1 AND state='queued'",
                rusqlite::params![request.message.as_str(), request.created_ms],
            )?;
            tx.commit()?.discard();
            return Ok(FollowupDecision::Consumed);
        }
        if queue::next(&tx, &stored.child)?.is_none_or(|next| next.message != *request.message) {
            return Ok(FollowupDecision::Waiting);
        }
        let chain = read_chain(&tx, &stored.child)?
            .ok_or_else(|| refused("followup target is no longer a child"))?;
        match chain.current.status {
            OperationStatus::Running(_) => return Ok(FollowupDecision::Waiting),
            OperationStatus::Admitted | OperationStatus::ExternallyAmbiguous(_) => {
                return Err(refused(
                    "followup target has unresolved admission or ambiguity",
                ))
            }
            OperationStatus::Terminal(_) => {}
        }
        let admission = request.admission;
        if admission.child != stored.child || admission.parent != chain.current.parent {
            return Err(refused(
                "followup run changed its child or original parent route",
            ));
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM subagent_operation WHERE child_session_id=?1",
            [stored.child.as_str()],
            |row| row.get(0),
        )?;
        if !(1..i64::try_from(MAX_RUNS).map_err(|_| refused("run bound overflow"))?)
            .contains(&count)
        {
            return Err(refused(
                "child exceeds its retained run bound; start a new agent",
            ));
        }
        let reused_execution: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family='session' AND aggregate_id=?1
             AND type LIKE 'session.execution.%'
             AND CASE WHEN json_valid(data) THEN json_extract(data,'$.executionID')=?2 ELSE 1 END)",
            (stored.child.as_str(), request.execution.as_str()),
            |row| row.get(0),
        )?;
        if reused_execution {
            return Err(refused(
                "followup cannot reuse a previous execution identity",
            ));
        }
        let pending = Inbox::pending_inputs_within(&tx, &stored.child)?
            .into_iter()
            .min_by_key(|input| {
                (
                    if input.delivery == DeliveryMode::Steer {
                        0
                    } else {
                        1
                    },
                    input.seq,
                )
            });
        let (input, seq, prompt, new_input) = match pending {
            Some(PendingInput {
                input, seq, text, ..
            }) => (input, seq, text, false),
            None => {
                let text = intent::wake_text(request.message);
                let metadata = serde_json::json!({"source":"agent-followup","messageID":request.message.as_str()});
                let outcome = Inbox::admit_synthetic_within(
                    &tx,
                    &held,
                    &SyntheticAdmitRequest {
                        event_id: request.input_event,
                        session: &stored.child,
                        input: request.input,
                        text: &text,
                        description: Some("Agent followup"),
                        metadata: Some(&metadata),
                        delivery: DeliveryMode::Queue,
                        resume: true,
                        created_ms: request.created_ms,
                    },
                )?;
                let AdmitOutcome::Admitted { seq, .. } = outcome else {
                    return Err(refused(
                        "followup wake input identity already belongs to another admission",
                    ));
                };
                (request.input.clone(), seq, text, true)
            }
        };
        Self::open_within(&tx, admission, request.created_ms)?;
        if tx.execute(
            START_OPERATION_SQL,
            rusqlite::params![
                admission.subagent.as_str(),
                generation_to_i64(request.generation)?,
                request.created_ms
            ],
        )? != 1
        {
            return Err(refused("fresh followup operation could not be claimed"));
        }
        tx.execute(
            "INSERT INTO subagent_run(operation_id,predecessor_id,child_session_id,
             execution_id,input_id,admitted_seq,cause_message_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                admission.subagent.as_str(),
                chain.current.operation.as_str(),
                stored.child.as_str(),
                request.execution.as_str(),
                input.as_str(),
                i64::try_from(seq).map_err(|_| refused("input position overflow"))?,
                request.message.as_str()
            ],
        )?;
        if tx.execute(
            "UPDATE subagent_followup SET state='started',operation_id=?2,time_updated=?3
            WHERE message_id=?1 AND state='queued'",
            rusqlite::params![
                request.message.as_str(),
                admission.subagent.as_str(),
                request.created_ms
            ],
        )? != 1
        {
            return Err(refused("followup wake changed before its run was bound"));
        }
        let binding = intent::run(&tx, &admission.subagent)?
            .ok_or_else(|| refused("fresh run binding is missing"))?;
        tx.commit()?.discard();
        Ok(FollowupDecision::Started {
            run: binding.run,
            prompt,
            new_input,
        })
    }
}

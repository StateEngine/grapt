//! Drive the interrupted turns startup recovery queued, and admit each one's
//! synthetic resume prompt once nothing newer has superseded it.
use super::*;
use grapt_api::SealedPrincipal;
use grapt_store::EventStore;

use crate::dogfood::startup_recovery::auto_resume::{
    self as recovery, PendingRow, ResumeState, StoredRow, SyntheticProgress, AUTO_RESUME_PAGE_SIZE,
    MAX_AUTO_RESUME_ATTEMPTS,
};

/// What admitting one synthetic resume prompt did.
pub(in crate::dogfood) enum AutoResumeAdmission {
    Submitted,
    Superseded(&'static str),
}

impl ServerApplication {
    #[allow(
        clippy::too_many_arguments,
        reason = "typed auto-resume admission coordinates"
    )]
    pub(in crate::dogfood) async fn submit_auto_resume(
        &self,
        principal: SealedPrincipal,
        session: SessionId,
        input: MessageId,
        turn_origin_seq: u64,
        text: String,
        description: Option<String>,
        metadata: Option<Value>,
    ) -> Result<AutoResumeAdmission, String> {
        let record = self.get_record(&session)?;
        let agent = grapt_types::AgentId::new(record.agent.unwrap_or_else(|| "default".to_owned()))
            .ok_or_else(|| "session agent identity is invalid".to_owned())?;
        self.ensure_session_writer(&session)?;
        subagent::claim_single_prompt(self, &session)?;
        let reservation = self.prompt_mailbox.reserve()?;
        let wake = self
            .wake_slots
            .reserve_for_workspace(&session, Path::new(record.placement.directory()))?;
        let event = self.mint_event()?;
        let created_ms = self.clock.now().as_millis();
        let (admission, seq) = {
            let mut inbox = self.lock_inbox();
            if let Some(reason) = recovery::verify_before_admission(
                inbox.store().connection(),
                &session,
                turn_origin_seq,
            )? {
                return Ok(AutoResumeAdmission::Superseded(reason));
            }
            let admission = inbox
                .admit_synthetic(grapt_inbox::SyntheticAdmitRequest {
                    event_id: &event,
                    session: &session,
                    input: &input,
                    text: &text,
                    description: description.as_deref(),
                    metadata: metadata.as_ref(),
                    delivery: DeliveryMode::Queue,
                    resume: true,
                    created_ms,
                })
                .map_err(|error| error.to_string())?;
            let seq = match &admission {
                AdmitOutcome::Admitted { seq, .. } => *seq,
                AdmitOutcome::Reconciled { .. } => {
                    inbox
                        .admission_coordinates(&input)
                        .map_err(|error| error.to_string())?
                        .seq
                }
            };
            if matches!(admission, AdmitOutcome::Admitted { .. }) {
                input_events::publish(&inbox, &self.hub, &session, seq);
                self.agent_activity.send_replace(());
            }
            (admission, seq)
        };
        if matches!(
            admission,
            AdmitOutcome::Reconciled {
                wake: grapt_inbox::WakeDisposition::Settled
            }
        ) {
            return Ok(AutoResumeAdmission::Superseded("retired_resume_input"));
        }
        if !should_execute(&admission) {
            return Ok(AutoResumeAdmission::Submitted);
        }
        let accepted = self.schedule_admitted(
            principal,
            session,
            input,
            seq,
            agent,
            reservation,
            Some(wake),
            None,
            None,
        )?;
        accepted
            .await
            .map_err(|_| "execution owner closed before accepting custody".to_owned())??;
        Ok(AutoResumeAdmission::Submitted)
    }
}

/// Background post-readiness driver: visits runnable `pending_resume` rows in
/// bounded pages, claims or reclaims each row durably before launching, and
/// atomically verifies no newer human input arrived while admitting the
/// deterministic synthetic resume prompt.
pub(in crate::dogfood) async fn drive(
    application: Arc<ServerApplication>,
    principal: SealedPrincipal,
) {
    let mut cursor = String::new();
    loop {
        let page = {
            let inbox = application.lock_inbox();
            recovery::list_runnable_after(
                inbox.store().connection(),
                &cursor,
                AUTO_RESUME_PAGE_SIZE,
            )
        };
        let rows = match page {
            Ok(rows) if rows.is_empty() => break,
            Ok(rows) => rows,
            Err(error) => {
                crate::server_log::warn(format_args!(
                    "startup automatic resume scan stopped: {error}"
                ));
                break;
            }
        };
        if let Some(last) = rows.last() {
            cursor = last.session.as_str().to_owned();
        }
        for row in rows {
            drive_one(&application, &principal, row).await;
            tokio::task::yield_now().await;
        }
    }
}

fn settle_drive_outcome(
    application: &ServerApplication,
    row: &PendingRow,
    attempt: u32,
    state: ResumeState,
    reason: &str,
) {
    let retired = {
        let mut inbox = application.lock_inbox();
        let now_ms = application.clock.now().as_millis();
        recovery::upsert_row(
            inbox.store_mut(),
            &row.session,
            row.turn_origin_seq,
            &row.interrupted_execution_id,
            attempt,
            state,
            Some(reason),
            now_ms,
        )
        .unwrap_or(0)
    };
    if retired > 0 {
        application.hub.invalidate_session_because(
            &row.session,
            &format!(
                "session {}: retired automatic resume input ({reason})",
                row.session
            ),
        );
    }
}

async fn drive_one(
    application: &Arc<ServerApplication>,
    principal: &SealedPrincipal,
    row: PendingRow,
) {
    let claim = {
        let mut inbox = application.lock_inbox();
        claim_row(application, inbox.store_mut(), &row)
    };
    let attempt = match claim {
        Ok(ClaimOutcome::Launch { attempt }) => attempt,
        Ok(ClaimOutcome::Skip) => return,
        Err(error) => {
            crate::server_log::warn(format_args!(
                "session {}: automatic resume claim failed: {error}",
                row.session
            ));
            return;
        }
    };
    let message_id =
        match recovery::synthetic_message_id(&row.session, row.turn_origin_seq, attempt) {
            Ok(id) => id,
            Err(error) => {
                crate::server_log::warn(format_args!(
                    "session {}: automatic resume message id failed: {error}",
                    row.session
                ));
                return;
            }
        };
    let text = format!(
        "The server restarted while your previous turn was in progress (interrupted execution {}; resume attempt {attempt} of {MAX_AUTO_RESUME_ATTEMPTS}). Any tool runs lost during the restart have been settled in history. Inspect the session state and continue the user's task without repeating completed side effects.",
        row.interrupted_execution_id
    );
    let metadata = json!({
        "kind": "auto_resume",
        "turnOriginSeq": row.turn_origin_seq,
        "interruptedExecutionID": row.interrupted_execution_id,
        "attempt": attempt,
        "maxAttempts": MAX_AUTO_RESUME_ATTEMPTS,
    });
    let outcome = application
        .submit_auto_resume(
            principal.clone(),
            row.session.clone(),
            message_id,
            row.turn_origin_seq,
            text,
            Some("Automatic resume after server restart".to_owned()),
            Some(metadata),
        )
        .await;
    match outcome {
        Ok(AutoResumeAdmission::Submitted) => {
            crate::server_log::info(format_args!(
                "session {}: automatic resume started origin_seq={} attempt={attempt}/{MAX_AUTO_RESUME_ATTEMPTS}",
                row.session, row.turn_origin_seq
            ));
        }
        Ok(AutoResumeAdmission::Superseded(reason)) => {
            settle_drive_outcome(application, &row, attempt, ResumeState::Completed, reason);
            crate::server_log::info(format_args!(
                "session {}: automatic resume superseded before admission reason={reason}",
                row.session
            ));
        }
        Err(error) => {
            settle_drive_outcome(
                application,
                &row,
                attempt,
                ResumeState::Stopped,
                "resume_submission_failed",
            );
            crate::server_log::warn(format_args!(
                "session {}: automatic resume submission failed: {error}",
                row.session
            ));
        }
    }
}

enum ClaimOutcome {
    Launch { attempt: u32 },
    Skip,
}

fn claim_next_attempt(
    application: &ServerApplication,
    store: &mut EventStore,
    session: &SessionId,
    current: &StoredRow,
    now_ms: i64,
) -> Result<ClaimOutcome, String> {
    if current.attempts >= MAX_AUTO_RESUME_ATTEMPTS {
        recovery::upsert_row(
            store,
            session,
            current.turn_origin_seq,
            &current.interrupted_execution_id,
            current.attempts,
            ResumeState::Stopped,
            Some("max_auto_resume_attempts_exceeded"),
            now_ms,
        )?;
        recovery::record_cap_status(store, &application.clock, &application.ids, session)?;
        return Ok(ClaimOutcome::Skip);
    }
    let next_attempt = current.attempts.saturating_add(1);
    let msg_id = recovery::synthetic_message_id(session, current.turn_origin_seq, next_attempt)?;
    recovery::upsert_row_with_input(
        store,
        session,
        current.turn_origin_seq,
        &current.interrupted_execution_id,
        next_attempt,
        ResumeState::Running,
        None,
        now_ms,
        Some(&msg_id),
    )?;
    Ok(ClaimOutcome::Launch {
        attempt: next_attempt,
    })
}

fn claim_row(
    application: &ServerApplication,
    store: &mut EventStore,
    row: &PendingRow,
) -> Result<ClaimOutcome, String> {
    let now_ms = application.clock.now().as_millis();
    let Some(current) = recovery::load_row(store.connection(), &row.session)? else {
        return Ok(ClaimOutcome::Skip);
    };
    if current.turn_origin_seq != row.turn_origin_seq {
        return Ok(ClaimOutcome::Skip);
    }
    if let Some(reason) = recovery::verify_before_admission(
        store.connection(),
        &row.session,
        current.turn_origin_seq,
    )? {
        let next_state = if reason.starts_with("superseded") {
            ResumeState::Completed
        } else {
            ResumeState::Stopped
        };
        let retired = recovery::upsert_row(
            store,
            &row.session,
            current.turn_origin_seq,
            &current.interrupted_execution_id,
            current.attempts,
            next_state,
            Some(reason),
            now_ms,
        )?;
        if retired > 0 {
            application.hub.invalidate_session_because(
                &row.session,
                &format!(
                    "session {}: retired stale automatic resume input",
                    row.session
                ),
            );
        }
        return Ok(ClaimOutcome::Skip);
    }
    match current.state {
        ResumeState::Pending => {
            claim_next_attempt(application, store, &row.session, &current, now_ms)
        }
        ResumeState::Running => {
            let msg_id = recovery::synthetic_message_id(
                &row.session,
                current.turn_origin_seq,
                current.attempts,
            )?;
            match recovery::synthetic_input_progress(store.connection(), &row.session, &msg_id)? {
                SyntheticProgress::Started => {
                    recovery::upsert_row(
                        store,
                        &row.session,
                        current.turn_origin_seq,
                        &current.interrupted_execution_id,
                        current.attempts,
                        ResumeState::Completed,
                        Some("already_started"),
                        now_ms,
                    )?;
                    Ok(ClaimOutcome::Skip)
                }
                SyntheticProgress::NotPromoted => {
                    recovery::upsert_row_with_input(
                        store,
                        &row.session,
                        current.turn_origin_seq,
                        &current.interrupted_execution_id,
                        current.attempts,
                        ResumeState::Running,
                        None,
                        now_ms,
                        Some(&msg_id),
                    )?;
                    Ok(ClaimOutcome::Launch {
                        attempt: current.attempts,
                    })
                }
                SyntheticProgress::PromotedNotStarted => {
                    claim_next_attempt(application, store, &row.session, &current, now_ms)
                }
            }
        }
        ResumeState::Completed | ResumeState::Stopped => Ok(ClaimOutcome::Skip),
    }
}

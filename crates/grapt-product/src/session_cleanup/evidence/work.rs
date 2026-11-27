//! Pure folds of the native work we can prove has settled. Unsupported event
//! shapes remain retention evidence; a terminal execution alone is not proof.
use std::collections::BTreeMap;

use grapt_event::EventKind;
use grapt_form::FormEvent;
use grapt_inbox::{InboxEvent, InboxFold};
use grapt_permission::{PermissionEvent, PermissionFold};
use grapt_session::{HistoryRow, MessageHistory, StepTerminal};
use grapt_types::{
    BlockerState, CancelledBy, ExternalEffectState, FormAnswer, FormBlockerState, FormId,
    SessionId, StepTerminalReason,
};
use serde::Deserialize;
use serde_json::Value;

use super::tools;

#[derive(Debug, Default)]
pub(super) struct WorkEvidence {
    inbox: InboxFold,
    permissions: PermissionFold,
    history: MessageHistory,
    tools: tools::Tools,
    forms: BTreeMap<FormId, FormBlockerState>,
    last_work: Option<u64>,
    execution_terminal: Option<u64>,
    ambiguous: bool,
    pub(super) observed_ms: i64,
}

impl WorkEvidence {
    pub(super) fn execution_settled(&mut self, seq: u64) {
        self.execution_terminal = Some(seq);
    }

    /// False means the event is not understood well enough to permit cleanup.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn observe(
        &mut self,
        session: &SessionId,
        seq: u64,
        raw_kind: &str,
        data: &Value,
    ) -> Result<bool, String> {
        let Ok(kind) = EventKind::parse(raw_kind) else {
            return Ok(false);
        };
        let name = kind.name();
        let version = kind.version().value();
        if data
            .get("sessionID")
            .is_some_and(|value| value.as_str() != Some(session.as_str()))
        {
            return Err("work payload identity differs from its aggregate".into());
        }
        match kind {
            EventKind::SessionInputAdmittedV1 | EventKind::SessionInputPromotedV1 => {
                let event = InboxEvent::decode(name, version, data)
                    .map_err(|error| error.to_string())?
                    .ok_or("missing inbox fact")?;
                // Agent/operation mail has a separate capture/ack lifecycle; the
                // pending-input fold deliberately does not settle that lifecycle.
                if !matches!(
                    event,
                    InboxEvent::Admitted { .. }
                        | InboxEvent::SyntheticAdmitted { .. }
                        | InboxEvent::Promoted { .. }
                ) {
                    return Ok(false);
                }
                self.inbox.apply(event, seq);
            }
            EventKind::PermissionAskedV1
            | EventKind::PermissionAskedV2
            | EventKind::PermissionRepliedV1
            | EventKind::PermissionRepliedV2
            | EventKind::PermissionSuspendedV1
            | EventKind::PermissionCancelledV1 => {
                let event = PermissionEvent::decode(name, version, data)
                    .map_err(|error| error.to_string())?
                    .ok_or("missing permission fact")?;
                self.permissions.apply(event);
            }
            EventKind::FormCreatedV1
            | EventKind::FormSuspendedV1
            | EventKind::FormRepliedV1
            | EventKind::FormCancelledV1 => {
                self.form(
                    FormEvent::decode(name, version, data)
                        .map_err(|error| error.to_string())?
                        .ok_or("missing form fact")?,
                )?;
            }
            EventKind::SessionAssistantMessageOpenedV1
            | EventKind::SessionAssistantMessageOpenedV2
            | EventKind::SessionContentBlockOpenedV1
            | EventKind::SessionContentAppendedV1
            | EventKind::SessionUsageObservedV1
            | EventKind::SessionUsageObservedV2
            | EventKind::SessionStepEndedV2
            | EventKind::SessionStepEndedV3
            | EventKind::SessionStepFailedV2
            | EventKind::SessionStepFailedV3 => {
                self.history
                    .apply(HistoryRow {
                        seq,
                        kind_name: name,
                        kind_version: version,
                        data,
                    })
                    .map_err(|error| error.to_string())?;
                self.tools.message(kind, data)?;
                if kind == EventKind::SessionContentAppendedV1 {
                    self.tools.content(data)?;
                }
                // Check failed terminals even when an opening is missing: the
                // history fold can legitimately read terminals without messages.
                let reason = match kind {
                    EventKind::SessionStepFailedV2 => Some(
                        grapt_event::session_step_failed::decode(data)
                            .map_err(|error| error.to_string())?
                            .reason()
                            .clone(),
                    ),
                    EventKind::SessionStepFailedV3 => {
                        let observed = grapt_event::session_step_observed::decode_failed(data)
                            .map_err(|error| error.to_string())?;
                        self.observed_ms =
                            self.observed_ms.max(observed.completed_at().as_millis());
                        Some(observed.fact().reason().clone())
                    }
                    EventKind::SessionStepEndedV3 => {
                        let observed = grapt_event::session_step_observed::decode_ended(data)
                            .map_err(|error| error.to_string())?;
                        self.observed_ms =
                            self.observed_ms.max(observed.completed_at().as_millis());
                        None
                    }
                    _ => None,
                };
                self.ambiguous |= reason.as_ref().is_some_and(uncertain);
            }
            EventKind::SessionToolInputStartedV1
            | EventKind::SessionToolInputStartedV2
            | EventKind::SessionToolNestedStartedV1
            | EventKind::SessionToolTerminalV1 => {
                self.tools.observe(session, kind, data)?;
                self.history
                    .apply(HistoryRow {
                        seq,
                        kind_name: name,
                        kind_version: version,
                        data,
                    })
                    .map_err(|error| error.to_string())?;
            }
            EventKind::SessionRequestCacheV1 => {
                grapt_event::request_cache::decode(data)?;
            }
            EventKind::SessionRequestContextV1 => {
                grapt_event::request_context::decode(data)?;
            }
            EventKind::SessionConfigBoundV1 => {
                let binding: Binding =
                    serde_json::from_value(data.clone()).map_err(|error| error.to_string())?;
                let bound: SessionId = binding
                    .session
                    .parse()
                    .map_err(|error| format!("invalid config session: {error}"))?;
                if bound != *session {
                    return Err("config binding identity differs".into());
                }
                return Ok(true);
            }
            EventKind::SessionInstructionsUpdatedV2 => {
                if data.as_object().is_none_or(|object| object.len() != 2)
                    || data.get("sessionID").and_then(Value::as_str) != Some(session.as_str())
                {
                    return Err("instruction delta envelope is malformed".into());
                }
                grapt_instruction::InstructionFold::default()
                    .apply(
                        session.clone(),
                        data.get("delta").ok_or("missing instruction delta")?,
                    )
                    .map_err(|error| error.to_string())?;
                return Ok(true);
            }
            // Legacy histories, compaction/revert/fork, native mail and future
            // versions have additional settlement/lineage semantics: retain.
            _ => return Ok(false),
        }
        self.last_work = Some(seq);
        Ok(true)
    }

    pub(super) fn unresolved(&self) -> Result<bool, String> {
        if !self.inbox.violations.is_empty()
            || !self.inbox.unmatched_promotions.is_empty()
            || !self.permissions.violations.is_empty()
            || !self.permissions.unmatched_references.is_empty()
            || self.history.orphaned_items() != 0
        {
            return Err("work history has missing or contradictory lifecycle evidence".into());
        }
        let permission = self.permissions.outcome();
        let messages = self.history.messages().map_err(|error| error.to_string())?;
        Ok(self.ambiguous
            || self.tools.unresolved()
            || !self.inbox.pending.is_empty()
            || self.forms.values().any(|state| !state.is_terminal())
            || permission.still_issued != 0
            || permission.still_suspended != 0
            || self
                .last_work
                .is_some_and(|seq| self.execution_terminal.is_none_or(|end| end < seq))
            || messages.iter().any(|message| match &message.terminal {
                None => true,
                Some(StepTerminal::Failed { reason }) => uncertain(reason),
                Some(StepTerminal::Ended { .. }) => false,
            }))
    }

    fn form(&mut self, event: FormEvent) -> Result<(), String> {
        match event {
            FormEvent::Created { id, .. } => {
                if self.forms.insert(id, BlockerState::Issued).is_some() {
                    return Err("duplicate form admission".into());
                }
            }
            FormEvent::Suspended { form, reason, .. } => {
                let state = self
                    .forms
                    .get_mut(&form)
                    .ok_or("form suspension without admission")?;
                if !matches!(state, BlockerState::Issued) {
                    return Err("form suspension without pending form".into());
                }
                *state = BlockerState::Suspended(reason);
            }
            FormEvent::Answered { form, answer, .. } => {
                let answer = FormAnswer::new(grapt_event::canonical_string(&answer))
                    .ok_or("invalid form answer")?;
                self.settle_form(form, BlockerState::Settled(answer))?;
            }
            FormEvent::Cancelled { form, .. } => {
                self.settle_form(form, BlockerState::Cancelled(CancelledBy::User))?;
            }
        }
        Ok(())
    }

    fn settle_form(&mut self, form: FormId, terminal: FormBlockerState) -> Result<(), String> {
        let state = self
            .forms
            .get_mut(&form)
            .ok_or("form terminal without admission")?;
        if state.is_terminal() {
            return Err("duplicate form terminal".into());
        }
        *state = terminal;
        Ok(())
    }
}

fn uncertain(reason: &StepTerminalReason) -> bool {
    matches!(
        reason,
        StepTerminalReason::Unknown { .. }
            | StepTerminalReason::DurabilityFailure
            | StepTerminalReason::ProcessLoss {
                effect: ExternalEffectState::Ambiguous { .. }
            }
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    #[serde(rename = "sessionID")]
    session: String,
    // Deserialize through the owner, without reading its selected file.
    #[serde(rename = "binding")]
    _binding: grapt_session::config_binding::ConfigBinding,
}

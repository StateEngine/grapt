//! Native mail at the existing provider safe boundary; no task or second inbox.
use super::{context::SessionCapabilities, stateengine_resume::ContinuityUnavailable as E};
mod convergence;
pub(super) mod custody;
mod endpoint;
pub(super) mod linked;
mod rebind;
mod recovery;
use grapt_api::u3a::{MailAttentionInfo, MailPendingCount};
use grapt_inbox::Inbox;
use grapt_instruction::InstructionService;
use grapt_mail_transport::operation::{ConnectionFailure, OperationCause, WriteEffect};
use grapt_runtime::mailbox::remote::credential::{
    CredentialBudget, CredentialEvent, Disposition, ProvisionGeneration, RefusalCause,
};
use grapt_runtime::mailbox::remote::{
    http::{http_mailbox, http_mailbox_v2, HttpMailbox},
    NativeMailBatch, RemoteMailboxError,
};
use grapt_store::{MailBindingExpectation, RemoteMailboxScope};
use grapt_types::{
    remote_mail::*, IdAuthority, InstructionKey, SeatRecipientId, Secret, SessionId, StepNumber,
};
use serde_json::Value;
use std::{
    num::NonZeroU16,
    sync::{Arc, Mutex},
};

struct Binding {
    raw: String,
    scope: RemoteMailboxScope,
    capability: Arc<Secret<String>>,
    /// The provision `capability` came from: every outcome is stamped with it.
    generation: ProvisionGeneration,
    protocol: progress::MailTransportVersion,
}

/// Render source positions for a terminal diagnostic.
///
/// The cursor type is opaque: it bounds length and non-emptiness, not the
/// character set. A valid cursor can therefore carry newlines, carriage returns
/// or terminal escape sequences, which must never be printed raw. Each position
/// is quoted and escaped, and they are separated by a space, so the boundary
/// between two positions cannot be forged by their content.
fn render_positions(positions: &[grapt_mail_transport::identity::ExclusiveSourceCursor]) -> String {
    positions
        .iter()
        .map(|position| format!("{:?}", position.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub(super) struct NativeMailContext {
    inbox: Arc<Mutex<Inbox>>,
    custody: Arc<Mutex<SessionCapabilities>>,
    session: SessionId,
    ids: Arc<dyn IdAuthority + Send + Sync>,
    clock: Arc<dyn grapt_types::ClockAuthority + Send + Sync>,
    port: Result<NonZeroU16, endpoint::Refusal>,
    service_root: Option<std::path::PathBuf>,
    service_owned: bool,
    runtime: Option<HttpMailbox>,
    advance: Option<convergence::Advance>,
    binding: Option<Binding>,
    batch: Option<NativeMailBatch>,
    prepared: bool,
    /// When a quiet subscription last proved it is still held by the door.
    pub(super) probed: Option<tokio::time::Instant>,
    /// Partial scans and overflow remain pending, never a fabricated empty inbox.
    pub(super) pending: bool,
    attention: MailAttentionInfo,
    verified_revision: Option<Arc<()>>,
    location: grapt_types::LocationRef,
    wake: grapt_runtime::mailbox::presentation::MailWake,
    close_pending: bool,
    /// Historical unconfirmed shutdown; never erased by a later connection.
    close_failed: bool,
    /// Keep invalid close receipts and failed local joins visible to the service owner.
    close_cleanup_failed: bool,
    blocked_revision: Option<Arc<()>>,
    /// Retry allowance and sticky refusal, owned by provision generation.
    credential: CredentialBudget,
    recovery_action: Option<recovery::Action>,
    /// The last advance failed with a refusal that may clear from the source side.
    slow_retry: bool,
    /// Stopped until a slow retry restores live mail; survives prompt waits and cancellation.
    slow_stopped: bool,
    wake_before: Option<grapt_types::MailboxAdmissionSeq>,
    /// The wake scan reads only rows above this mark.
    wake_floor: grapt_types::MailboxCursor,
    /// The newest row of the wake pass in progress.
    wake_top: Option<grapt_types::MailboxAdmissionSeq>,
    subscription_open: bool,
    wake_scan_requested: bool,
}

impl NativeMailContext {
    /// New actionable mail restarts the wake scan from the newest row; a
    /// witnessed delivery also drops its mark below the delivered row.
    fn rescan_wake(&mut self, slice: &grapt_runtime::mailbox::remote::RemoteMailboxSlice) {
        if slice.rescan_notifications {
            self.wake_before = None;
            self.wake_scan_requested = true;
        }
        if let Some(from) = slice.delivered_from {
            self.wake_floor = self
                .wake_floor
                .min(grapt_runtime::mailbox::remote::below(from));
        }
    }
    fn transport_refusal(&mut self, stage: &str, error: RemoteMailboxError) -> E {
        // Keep the closed cause, never source JSON, cursor or credential bytes.
        let event = recovery::credential_event(&error);
        let disposition = self.observe_credential(event);
        if event == CredentialEvent::ProjectMismatch && disposition != Disposition::Ignore {
            self.close_pending = true;
            crate::diagnostics::report(format!("native mail project binding refused session={} stage={stage}; repair this Session's stateengine-identity project and reprovision its capability; delivery debt retained", self.session));
            E::Principal
        } else {
            crate::diagnostics::report(format!("native mail unavailable session={} stage={stage} cause={error}; delivery debt retained", self.session));
            E::Transport
        }
    }

    #[cfg(test)]
    fn use_test_port(&mut self, port: NonZeroU16) {
        self.service_root = None;
        self.port = Ok(port);
    }

    pub(super) fn new(
        session: SessionId,
        inbox: Arc<Mutex<Inbox>>,
        custody: Arc<Mutex<SessionCapabilities>>,
        ids: Arc<dyn IdAuthority + Send + Sync>,
        clock: Arc<dyn grapt_types::ClockAuthority + Send + Sync>,
    ) -> Result<Self, E> {
        let location =
            grapt_session::SessionStore::over(inbox.lock().map_err(|_| E::Binding)?.store())
                .record(&session)
                .map_err(|_| E::Binding)?
                .placement;
        Ok(Self {
            inbox,
            custody,
            session,
            ids,
            clock,
            port: Err(endpoint::Refusal::Missing),
            service_root: None,
            service_owned: false,
            runtime: None,
            advance: None,
            binding: None,
            batch: None,
            prepared: false,
            probed: None,
            pending: false,
            attention: MailAttentionInfo::Unavailable,
            verified_revision: None,
            location,
            wake: Default::default(),
            close_pending: false,
            close_failed: false,
            close_cleanup_failed: false,
            blocked_revision: None,
            credential: CredentialBudget::default(),
            recovery_action: None,
            slow_retry: false,
            slow_stopped: false,
            wake_before: None,
            wake_floor: grapt_types::MailboxCursor::START,
            wake_top: None,
            subscription_open: false,
            wake_scan_requested: true,
        })
    }

    fn current_binding(&self) -> Result<Option<Binding>, E> {
        let inbox = self.inbox.lock().map_err(|_| E::Binding)?;
        let entries =
            InstructionService::entries(inbox.store(), &self.session).map_err(|_| E::Binding)?;
        let Some(entry) = entries
            .into_iter()
            .find(|e| e.key.as_str() == "stateengine-identity")
        else {
            return Ok(None);
        };
        let v: Value = serde_json::from_str(&entry.value).map_err(|_| E::Binding)?;
        if v.get("scope").and_then(Value::as_str) == Some("incognito") {
            return Ok(None);
        }
        let project = v
            .get("project_id")
            .and_then(Value::as_str)
            .ok_or(E::Binding)?;
        let agent = v
            .get("agent_id")
            .and_then(Value::as_str)
            .ok_or(E::Binding)?;
        let scope = RemoteMailboxScope {
            projects: ProjectPair::new(
                SourceProjectId::try_new(project).map_err(|_| E::Binding)?,
                TargetProjectId::try_new(project).map_err(|_| E::Binding)?,
            ),
            principal: SourcePrincipalId::try_new(agent).map_err(|_| E::Binding)?,
            recipient: SeatRecipientId::try_new(agent).map_err(|_| E::Binding)?,
        };
        let custody = self.custody.lock().map_err(|_| E::Capability)?;
        let capability = custody
            .values
            .get(&self.session)
            .cloned()
            .ok_or(E::Capability)?;
        let generation = custody
            .generations
            .get(&self.session)
            .copied()
            .ok_or(E::Capability)?;
        let protocol = match v.get("mail_transport_protocol") {
            None => progress::MailTransportVersion::V1,
            Some(value) => progress::MailTransportVersion::parse(value.as_str().ok_or(E::Binding)?)
                .ok_or(E::Binding)?,
        };
        Ok(Some(Binding {
            protocol,
            raw: entry.value,
            scope,
            capability,
            generation,
        }))
    }

    fn require_current(&self) -> Result<(), E> {
        match (&self.binding, self.current_binding()?) {
            (None, None) => Ok(()),
            (Some(old), Some(current))
                if old.raw == current.raw
                    && old.scope == current.scope
                    && Arc::ptr_eq(&old.capability, &current.capability) =>
            {
                Ok(())
            }
            _ => Err(E::Rebound),
        }
    }

    /// A delivery-contract refusal needs attention now and is reobserved after
    /// one slow wait on the owner's timer. Service and prompt paths share this.
    pub(in crate::dogfood::native_mail) fn stop_for_slow_retry(
        &mut self,
        recovery: &mut recovery::Recovery,
    ) {
        self.close_pending = true;
        self.slow_stopped = true;
        self.attention = MailAttentionInfo::Stopped;
        recovery.defer_slow(&self.session);
    }

    async fn converge(&mut self) -> Result<bool, E> {
        self.recovery_action = None;
        self.slow_retry = false;
        self.location =
            grapt_session::SessionStore::over(self.inbox.lock().map_err(|_| E::Binding)?.store())
                .record(&self.session)
                .map_err(|_| E::Binding)?
                .placement;
        if self.owns_runtime() {
            self.require_current()?;
        } else {
            self.binding = self.current_binding().inspect_err(|error| {
                if matches!(error, E::Capability) {
                    self.require_rebind();
                }
            })?;
            if let Some(binding) = &self.binding {
                // The latest trusted provision, however many came between.
                self.credential.provisioned(binding.generation);
            }
            let Some(binding) = &self.binding else {
                self.pending = false;
                self.attention = MailAttentionInfo::Disabled;
                return Ok(false);
            };
            // A retained owner may outlive a provision/rebind. Resolve the
            // Session's selected file only when creating its new transport.
            let selected = match &self.service_root {
                Some(service) => endpoint::read(
                    &self.inbox,
                    &self.session,
                    std::path::Path::new(self.location.directory()),
                    service,
                ),
                None => self.port,
            };
            self.require_current()?;
            let port = selected.map_err(|reason| {
                crate::diagnostics::report(format!("native mail unavailable session={} stage=endpoint cause={reason}; delivery debt retained", self.session));
                E::Transport
            })?;
            let client = MailTransportClientInstanceId::try_new(format!(
                "native-{}",
                self.ids.mint().as_str()
            ))
            .map_err(|_| E::Contract)?;
            self.runtime = Some(
                match binding.protocol {
                    progress::MailTransportVersion::V1 => http_mailbox(
                        port,
                        binding.capability.clone(),
                        self.inbox.clone(),
                        binding.scope.clone(),
                        client,
                    ),
                    progress::MailTransportVersion::V2 => http_mailbox_v2(
                        port,
                        binding.capability.clone(),
                        self.inbox.clone(),
                        binding.scope.clone(),
                        client,
                        self.clock.clone(),
                    )
                    .and_then(|runtime| {
                        // Only retained service custody owns advisory observation
                        // between independent delivery-settlement slices.
                        if self.service_owned {
                            runtime.with_interleaved_delivery()
                        } else {
                            Ok(runtime)
                        }
                    }),
                }
                .map_err(|error| self.transport_refusal("initialize", error))?,
            );
        }
        self.pending = true;
        let slice = match self.advance_slice().await {
            Ok(slice) => slice,
            Err(error) => {
                self.recovery_action = recovery::action(&error);
                self.slow_retry = self.recovery_action.is_none() && recovery::slow(&error);
                return Err(self.transport_refusal("advance", error));
            }
        };
        self.require_current()?;
        self.rescan_wake(&slice);
        if slice.live {
            self.observe_credential(CredentialEvent::Live);
        }
        if slice.live && !self.subscription_open {
            crate::diagnostics::report(format!(
                "native mail subscription opened session={}",
                self.session
            ));
            self.subscription_open = true;
        }
        Ok(slice.live)
    }

    fn attention_snapshot(&self, live: bool) -> Result<MailAttentionInfo, E> {
        if self.binding.is_none() {
            return Ok(MailAttentionInfo::Disabled);
        }
        if !live {
            return Ok(MailAttentionInfo::Unavailable);
        }
        self.require_current()?;
        let binding = self.binding.as_ref().ok_or(E::Binding)?;
        let inbox = self.inbox.lock().map_err(|_| E::Binding)?;
        let page = inbox
            .store()
            .read_remote_mailbox_for_binding_rows(
                &binding.scope,
                grapt_types::MailboxCursor::START,
                64,
                1048576,
                grapt_store::HistoryRows::Scan,
            )
            .map_err(|_| E::Transport)?;
        let count = u8::try_from(page.records.iter().filter(|r| r.actionable()).count())
            .map_err(|_| E::Contract)?;
        Ok(MailAttentionInfo::Ready {
            pending: MailPendingCount::new(count).ok_or(E::Contract)?,
            incomplete: page.next_after.is_some()
                || self
                    .runtime
                    .as_ref()
                    .is_some_and(HttpMailbox::delivery_debt_pending),
            decode_failures: MailPendingCount::new(
                u8::try_from(
                    self.runtime
                        .as_ref()
                        .map_or(0, |runtime| runtime.recorded_decode_failures())
                        .min(64),
                )
                .map_err(|_| E::Contract)?,
            )
            .ok_or(E::Contract)?,
        })
    }

    pub(super) async fn refresh(&mut self, step: StepNumber) -> Result<(), E> {
        self.prepared = false;
        self.batch = None;
        // Finish any service-owned slice before requesting this prompt's scan.
        // Its transport operation survives cancellation of either caller.
        if self.advance.is_some() {
            self.converge().await?;
        }
        if let Some(runtime) = &mut self.runtime {
            runtime.request_snapshot();
        }
        let live = self.converge().await?;
        self.prepare_observed(step, live)
    }

    /// The service owns transport progress. Preparation uses only its settled
    /// snapshot, leaving incomplete delivery visibly pending for the next wake.
    fn prepare_retained(&mut self, step: StepNumber) -> Result<(), E> {
        self.prepared = false;
        self.batch = None;
        if self.owns_runtime() {
            self.require_current()?;
        } else {
            self.binding = self.current_binding()?;
        }
        let live = self.runtime.as_ref().is_some_and(HttpMailbox::is_live);
        self.pending = self.binding.is_some() && !live;
        self.prepare_observed(step, live)
    }

    fn prepare_observed(&mut self, step: StepNumber, live: bool) -> Result<(), E> {
        self.attention = self.attention_snapshot(live)?;
        if live {
            let binding = self.binding.as_ref().ok_or(E::Binding)?;
            let expected = MailBindingExpectation::new(
                InstructionKey::new("stateengine-identity").ok_or(E::Contract)?,
                binding.raw.clone(),
            )
            .map_err(|_| E::Binding)?;
            let scan = self
                .runtime
                .as_mut()
                .ok_or(E::Incomplete)?
                .prepare_notifications(&self.session, step, expected)
                .map_err(|_| E::Transport)?;
            self.pending = scan.pending
                || self
                    .runtime
                    .as_ref()
                    .is_some_and(HttpMailbox::delivery_debt_pending);
            self.batch = scan.batch;
        }
        self.prepared = true;
        Ok(())
    }

    /// StepDriver joins this after dropping a cancelled preparation future.
    /// Close itself has the accepted five-second bound and ignores user cancel.
    pub(super) async fn finish(&mut self) -> Result<(), String> {
        self.close_transport().await.map_err(|_| "Native inbox shutdown could not be confirmed; retry the bound StateEngine connection.".to_owned())
    }

    /// Keep the typed close result until the owner decides whether a fresh
    /// observation is permitted. The historical missing receipt stays recorded.
    async fn close_transport(&mut self) -> Result<(), RemoteMailboxError> {
        let advanced = self.stop_advance().await;
        let closed = match self.runtime.as_mut() {
            Some(runtime) => runtime.close().await.map(|_| ()),
            None => Ok(()),
        };
        let closed = closed.and(advanced);
        self.runtime = None;
        if std::mem::take(&mut self.subscription_open) {
            crate::diagnostics::report(format!(
                "native mail subscription dropped session={} confirmed={}",
                self.session,
                closed.is_ok()
            ));
        }
        self.wake_before = None;
        self.wake_floor = grapt_types::MailboxCursor::START;
        self.wake_top = None;
        self.wake_scan_requested = true;
        self.close_failed |= closed.is_err();
        // The HTTP port consumes its local binding before requesting remote close.
        // A vanished door or explicitly absent binding loses that receipt, not
        // local cleanup. Keep the history; malformed replies, exceeded budgets
        // and uncertain effects remain failures.
        self.close_cleanup_failed |= closed.as_ref().err().is_some_and(|error| {
            !matches!(error, RemoteMailboxError::Port(failure)
                if matches!(failure.cause,
                    OperationCause::Connection(ConnectionFailure::Unavailable)
                    | OperationCause::Transport(grapt_mail_transport::TransportFailure::Disconnected | grapt_mail_transport::TransportFailure::TransportUnavailable)
                    | OperationCause::Subscription(grapt_mail_transport::SubscriptionClosed::Disconnected))
                    && failure.effect == WriteEffect::None)
        });
        closed
    }

    /// Record undecodable rows the source reported, as a diagnostic rather than
    /// a notice to the reader.
    ///
    /// The count and its positions describe rows the *source* could not hand
    /// over. The reader cannot act on that, and a permanent banner claiming
    /// their own mail was affected is a false alarm: the mail this window shows
    /// is exactly what the source admitted. So this reports where it went, and
    /// returns nothing for the conversation.
    pub(super) fn decode_warning(&self) -> Result<Option<String>, E> {
        self.require_current()?;
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(None);
        };
        let count = runtime.recorded_decode_failures();
        if count == 0 {
            return Ok(None);
        }
        // Positions are cursors the source chose; they name a place in its
        // stream and carry no message content. Capped so a noisy source cannot
        // grow the line without bound.
        let positions = render_positions(runtime.decode_failure_positions());
        crate::diagnostics::report(format!(
            "native mail: source reported {count} undecodable {} at positions [{positions}]; admitted mail is unaffected",
            if count == 1 { "row" } else { "rows" }
        ));
        Ok(None)
    }

    pub(super) fn take_batch(&mut self) -> Result<Option<NativeMailBatch>, E> {
        if !std::mem::take(&mut self.prepared) {
            return Err(E::Incomplete);
        }
        self.require_current()?;
        Ok(self.batch.take())
    }
}

#[cfg(test)]
mod tests;

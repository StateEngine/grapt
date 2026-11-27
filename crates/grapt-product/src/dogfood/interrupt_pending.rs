//! Esc stops the current owner before already-admitted human input can restart.
use super::*;

impl DogfoodApplication {
    /// What any stop owes after an execution has been interrupted.
    ///
    /// An interrupted execution is not a stopped Session. A native mail signal
    /// that arrives next would find no live waiter to cancel and start the turn
    /// again, so a stop is only complete once the durable hold records that
    /// later wakes wait for an explicit resume. Cancelling the waiters drops the
    /// reservations already made; the hold is what keeps a later signal from
    /// promoting at all.
    ///
    /// Every stop goes through here — the explicit interrupt and the close
    /// grace that follows a window leaving — so neither path can keep the other's
    /// line. Only a user's explicit resume clears the hold.
    pub(super) fn hold_after_stop(&self, session: &SessionId) -> Result<(), String> {
        self.wake_slots.cancel_waiters(session);
        self.ensure_session_writer(session).and_then(|()| {
            self.lock_inbox()
                .hold_native_mail_wakes(session, self.clock.now().as_millis())
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    /// The complete stop the last window's close owes its Session.
    ///
    /// `ExecutionBackend::close` stops what is running and drops the waiters
    /// that already exist. That alone does not keep the turn stopped: a native
    /// mail signal arriving afterwards finds nothing to cancel, promotes, and
    /// starts the turn again with no window present. So the close also records
    /// the same durable hold an explicit stop does, and only a user's explicit
    /// resume clears it.
    pub(super) fn close_after_last_window(&self, session: &SessionId) {
        // A stop that cannot address an execution has still ended the window's
        // claim; the Session stays resumable either way, and the hold is what a
        // later wake must respect.
        if let Err(error) = self.executor.close(session) {
            crate::server_log::warn(format_args!(
                "close grace warning reason=interrupt-unaddressed detail={error}"
            ));
        }
        // Recorded after the interrupt so a wake cannot slip between the two:
        // anything promoted while we were stopping is either left pending by
        // the hold or was already cancelled with the waiters. A hold that could
        // not be recorded is reported rather than dropped: the turn is stopped
        // now, but the next mail signal may start it again.
        if let Err(error) = self.hold_after_stop(session) {
            crate::server_log::warn(format_args!(
                "close grace warning reason=native-wake-hold-unrecorded detail={error}"
            ));
        }
    }

    pub(super) fn interrupt_with_pending(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: InterruptRequestDto,
    ) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
        let session = session_id.into_inner();
        if self.get_record(&session).is_err() {
            return ready(Err(SessionLookupApplicationError::SessionNotFound(
                not_found(&session),
            )));
        }
        let stop_processes = request
            .stop_processes()
            .as_present()
            .copied()
            .unwrap_or(false);
        let resume = request
            .resume_pending()
            .as_present()
            .copied()
            .unwrap_or(false);
        let execution = request
            .execution_id()
            .as_value()
            .map(|wire| wire.as_inner());
        // Repeated callers share the service-owned receipt without taking
        // another control slot or signalling the execution a second time.
        let (mut fence, control) =
            match self
                .wake_slots
                .reserve_stop(&session, stop_processes, resume, execution, || {
                    // Secure cleanup custody before cancellation becomes visible.
                    let control = self.prompt_mailbox.reserve_control()?;
                    let target = self.executor.interrupt(&session, execution)?;
                    Ok((target, control))
                }) {
                Ok(execution::StopAdmission::Owner(fence, control)) => (fence, control),
                Ok(execution::StopAdmission::Joined(receipt)) => {
                    return Box::pin(async move {
                        receipt.wait().await.map_err(|reason| {
                            SessionLookupApplicationError::InvalidRequest(invalid(reason))
                        })?;
                        Ok(NoContent)
                    });
                }
                Err(reason) => {
                    return ready(Err(SessionLookupApplicationError::InvalidRequest(invalid(
                        reason,
                    ))))
                }
            };
        if stop_processes {
            self.executor.cancel_processes(&session);
        }
        let held = self.hold_after_stop(&session);
        let app = self.clone();
        let response = fence.receipt();
        // The driver, not the HTTP request, owns the fence and all cleanup waits.
        control.submit_control(Box::pin(async move {
            let drained = fence.drained().await;
            let mut failures = Vec::new();
            if let Err(reason) = &drained { failures.push(reason.clone()); }
            if let Err(reason) = held { failures.push(format!("native wake hold could not be saved: {reason}")); }
            if app.executor.active_executions().iter().any(|(active, _)| active == &session) {
                failures.push("Session execution is still listed as running".into());
            }
            drop(drained);
            let mut processes_stopped = false;
            let wake = loop {
                if fence.processes_requested() && !processes_stopped {
                    app.executor.cancel_processes(&session);
                    if let Err(reason) = app.executor.stop_terminals(&session).await {
                        failures.push(reason);
                    }
                    processes_stopped = true;
                }
                // The locked boundary includes a late cleanup or resume join
                // or makes a subsequent stop target newly admitted work.
                match fence.release_admission(
                    processes_stopped,
                    failures.is_empty().then_some(&session),
                ) {
                    Ok(execution::StopRelease::ProcessesRequested) => continue,
                    Ok(execution::StopRelease::Released(wake)) => break wake,
                    Err(reason) => {
                        failures.push(reason);
                        break None;
                    }
                }
            };
            let result = if !failures.is_empty() {
                Err(format!("Session stop incomplete: {}. Retry stop to confirm cleanup before relaunching.", failures.join("; ")))
            } else if let Some(wake) = wake {
                app.resume_interrupted_pending(principal, session, wake).await
            } else {
                Ok(())
            };
            // A disconnected client does not cancel the service-owned stop.
            fence.finish(result);
        }));
        Box::pin(async move {
            response
                .wait()
                .await
                .map(|()| NoContent)
                .map_err(|reason| SessionLookupApplicationError::InvalidRequest(invalid(reason)))
        })
    }

    async fn resume_interrupted_pending(
        &self,
        principal: SealedPrincipal,
        session: SessionId,
        wake: execution::WakeReservation,
    ) -> Result<(), String> {
        let record = self.get_record(&session)?;
        let pending = self
            .lock_inbox()
            .pending_inputs(&session)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|row| {
                row.kind == grapt_inbox::PromotedInputKind::User
                    && row
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.get("origin"))
                        .and_then(Value::as_str)
                        != Some("seat-startup")
            });
        let Some(pending) = pending else {
            return Ok(());
        };
        let agent = grapt_types::AgentId::new(record.agent.unwrap_or_else(|| "default".into()))
            .ok_or("session agent identity is invalid")?;
        let resumed = (|| {
            let reservation = self.prompt_mailbox.reserve()?;
            let wake = self
                .wake_slots
                .bind_reservation(wake, Path::new(record.placement.directory()))?
                .after_interrupt();
            self.schedule_admitted(
                principal,
                session,
                pending.input,
                pending.seq,
                agent,
                reservation,
                Some(wake),
                None,
                None,
            )
        })()
        .map_err(|reason| format!("Session stopped; the message remains queued: {reason}"))?;
        match resumed
            .await
            .map_err(|_| "execution owner closed; the message remains queued".to_owned())??
        {
            None => Ok(()),
            Some(reason) => Err(reason),
        }
    }
}

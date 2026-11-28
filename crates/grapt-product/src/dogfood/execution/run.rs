//! One owned wake settles its execution and child callbacks before releasing custody.
use super::*;

impl LiveExecutor {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub async fn run(
        &self,
        session: SessionId,
        admission_seq: u64,
        principal: SealedPrincipal,
        agent: AgentId,
        startup: (
            ExecutionId,
            super::super::prompt_driver::Started,
            WakeCustody,
            tokio::sync::watch::Receiver<bool>,
            MessageId,
            Option<super::super::host_control::ExternalNoticeFence>,
        ),
    ) -> Result<Option<WakeOutcome>, ExecutionRefusal> {
        let (execution, started, wake, shutdown, input, external_notice) = startup;
        // A block before the acknowledgement happens before the input is
        // promoted: the prompt stays admitted and queued for the Session's
        // next turn, and the block is a not-run-yet notice, not a refused
        // prompt (the oracle keeps it pending too).
        let observed_start = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut wake_cancelled = wake.cancellation();
        let restarting = wake.restarting();
        let entered = async {
            let Some(mut wake) = wake.enter(&session, &started, shutdown).await? else {
                return Ok(None);
            };
            if task_application::already_promoted(&self.inbox, &session, &input)? {
                return Ok(None);
            }
            let max = super::super::MAX_ACTIVE_EXECUTIONS;
            let active = self
                .active
                .register(session.clone(), execution.clone(), max)?;
            let location = self.location.resolved(&session).await?;
            let checked = self.location.preflight(&session)?;
            let roots = super::super::instructions::InstructionRoots::discover(
                &location,
                &self.plugins,
                agent.as_str(),
            )
            .await?;
            let workspace = wake.bind_workspace(&self.wake_slots, &checked.tool_root)?;
            let registry_key = super::super::session_config::registry_key(
                &self.inbox,
                &session,
                checked.record.placement.directory(),
            )?;
            let mcp_location = self.mcp.hold_location(&registry_key).await.ok_or_else(|| {
                "MCP location capacity reached; this turn remains queued".to_owned()
            })?;
            let marks = (
                started.clone(),
                execution.clone(),
                Arc::clone(&observed_start),
                workspace,
                input.clone(),
                external_notice,
            );
            let custody = self
                .principals
                .register(session.clone(), principal, agent, roots, marks)
                .map_err(ExecutionRefusal::reason)?;
            Ok::<_, String>(Some((wake, active, custody, mcp_location)))
        };
        // Reservation cancellation also covers the acquired-but-unregistered
        // gap and asynchronous preflight, before an active signal exists.
        let entered = tokio::select! {
            biased;
            _ = wake_cancelled.wait_for(|stopped| *stopped) =>
                super::wake_custody::WakeReservation::cancelled_result(restarting).map(|_| None),
            result = entered => result,
        };
        let (_wake, _active, _principal_custody, _mcp_location) = match entered {
            Ok(Some(held)) => held,
            Ok(None) => {
                started.settle(Ok(()));
                return Ok(None);
            }
            Err(reason) => return self.not_run_yet(&session, &execution, &input, &started, reason),
        };
        // From BEFORE the acknowledgement until the drain ends, clients see
        // this Session as active, and an interrupt reaches it even before its
        // drain exists: no acknowledged caller can interrupt into a gap.
        let running = async {
            let (_preparing, mut cancelled) = self.active.prepare(session.clone());
            // Everything above refuses synchronously; the prompt response waits
            // for it. It does not wait for MCP startup: each server's startup and
            // catalog phases are configurable up to a day, so no client allowance
            // could cover them truthfully. Whatever fails from here on is recorded
            // as this execution's failure (below), as for a queued admission.
            started.settle(Ok(()));
            let child_hooks = self.child_hooks(&session, &execution);
            let child_started = match &child_hooks {
            Ok(Some(hooks)) => tokio::select! {
                biased;
                _ = cancelled.wait_for(|interrupted| *interrupted) => Ok(Vec::new()),
                result = hooks.fire(super::super::hooks::Event::SubagentStart, &session, None) => result,
            }
            .map(|_| ()),
            Ok(None) => Ok(()),
            Err(reason) => Err(reason.clone()),
        };
            // Snapshot the MCP catalog BEFORE planning begins. This is the async
            // boundary; everything after it plans synchronously. An interrupt
            // stops a slow MCP startup here instead of waiting it out.
            let mut unavailable = None;
            if child_started.is_ok() {
                tokio::select! {
                    biased;
                    _ = cancelled.wait_for(|interrupted| *interrupted) => {}
                    refreshed = self.refresh_published_tools(&session) => unavailable = refreshed,
                }
            }
            // Recorded when the chain opens, after the promotion: before the
            // answer, after the message it answers. An interrupted refresh
            // found nothing to report.
            if let Some(unavailable) = unavailable {
                self.principals.hold_mcp_notice(&session, unavailable);
            }
            let _snapshot = PublishedSnapshotGuard {
                snapshots: self.published.clone(),
                session: session.clone(),
            };
            self.location.compaction_recovery.take_failure(&session);
            let (woke, scoped_interrupted) = match child_started {
                Ok(()) => self.wake_task(&session, &mut cancelled).await,
                Err(reason) => (
                    Err(grapt_runtime::CoordinatorError::Run {
                        session: session.as_str().into(),
                        reason,
                    }),
                    false,
                ),
            };
            let failure =
                woke.as_ref()
                    .err()
                    .map_or(grapt_session::execution::Failure::Generic, |error| {
                        observations::public_failure_with_compaction(
                            &self.location,
                            &session,
                            error,
                        )
                    });
            if let Err(error) = &woke {
                self.note_context_stop(&session, &execution, error);
            }
            let result = woke.map_err(|error| ExecutionRefusal::Run(error.to_string()));
            self.report_dispatch_samples();
            self.location.persist_calibration(&session);
            let was_interrupted = self.interrupted.take(&session) || scoped_interrupted;
            #[cfg(feature = "handover-crash-fixture")]
            if was_interrupted {
                crate::fixture_seam::hold(crate::fixture_seam::HoldPoint::ExecutionInterrupted)
                    .await;
            }
            if result.is_err() && !observed_start.load(std::sync::atomic::Ordering::Acquire) {
                // A waiting admission may already be acknowledged. If preparing
                // its chain fails, record the execution that tried and failed;
                // this does not assert that an LLM Step or provider call existed.
                if self
                    .observe_execution(&session, &execution, None, &failure)
                    .is_ok()
                {
                    observed_start.store(true, std::sync::atomic::Ordering::Release);
                } else {
                    self.hub.invalidate_session_because(
                        &session,
                        &format!("session {session}: failed execution start could not be recorded"),
                    );
                }
            }
            let observed = if result.is_err() {
                grapt_session::execution::Outcome::Failed
            } else if was_interrupted {
                grapt_session::execution::Outcome::Interrupted
            } else {
                grapt_session::execution::Outcome::Succeeded
            };
            let settle_span = timing::session_context(
                &self.location.timing,
                &session,
                grapt_types::timing::TimingClass::Execution,
            )
            .span(grapt_types::timing::TimingStage::ExecutionSettle);
            let terminal = if observed_start.load(std::sync::atomic::Ordering::Acquire) {
                self.observe_execution(&session, &execution, Some(observed), &failure)
            } else {
                Ok(observations::ObservationPublication::Unavailable)
            };
            if let Err(error) = &terminal {
                self.hub.invalidate_session_because(
                    &session,
                    &format!(
                        "session {session}: execution terminal could not be recorded: {error}"
                    ),
                );
            }
            drop(settle_span);
            self.report_tail_samples();
            // This observer follows settlement even when preparation or the provider failed.
            // Its failure cannot turn an already completed child into a retry.
            if let Ok(Some(hooks)) = child_hooks {
                if let Err(reason) = hooks
                    .fire(super::super::hooks::Event::SubagentStop, &session, None)
                    .await
                {
                    crate::diagnostics::report(format!(
                        "session {session}: SubagentStop hook failed after settlement: {reason}"
                    ));
                }
            }
            let outcome = result.map_err(|refusal| match (refusal, &terminal) {
                (
                    ExecutionRefusal::Run(reason),
                    Ok(observations::ObservationPublication::Published),
                ) => ExecutionRefusal::PublishedFailure(reason),
                (other, _) => other,
            })?;
            terminal?;
            // Session-scoped diagnostics are consumed by their own run and never
            // promoted into provider/runtime failure after commit.
            let _advisory = self.publication.take(&session);
            let _admitted_input = admission_seq;
            Ok(Some(outcome))
        };
        tokio::pin!(running);
        tokio::select! {
            biased;
            result = &mut running => result,
            _ = async { drop(wake_cancelled.wait_for(|stopped| *stopped).await); } => {
                self.interrupt(&session, None)?;
                running.await
            }
        }
    }
}

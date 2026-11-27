//! Bounded physical-attempt execution, retry and history rebuild for one Step.
use super::*;

impl StepDriver {
    /// Run one Step, retrying and rebuilding under policy.
    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub async fn run<S, H, T, O>(
        step: StepNumber,
        policy: StepPolicy,
        source: &mut S,
        history: &mut H,
        tools: Arc<T>,
        mut owner: O,
        cancellation: Cancellation,
    ) -> (O, StepReport)
    where
        S: AttemptSource,
        H: HistorySource,
        T: ToolRunner,
        O: AttemptOwner,
    {
        let mut cancel = cancellation.signal();
        let mut decision_waits = tools.decision_waits();
        // `watch::Receiver::changed()` returns Err immediately and repeatedly
        // once every sender is dropped. Ignoring that Result would make the
        // cancellation arm always-ready: the backoff would be skipped and the
        // watchdog would spin. A dropped handle means "interrupt unavailable",
        // so the branch is disabled and timing continues normally.
        let mut interrupt_available = true;
        let mut receipts: Vec<AttemptReceipt> = Vec::new();
        let mut ordinal = 0u32;
        let mut rebuilds = 0u32;
        let mut preparation_timeouts = 0u32;
        let mut history_reloads = 0u32;
        let mut generation = history.generation();
        let mut capacity_wait = admission::CapacityWait::default();

        let result = loop {
            // Checked BEFORE minting an identity or preparing a factory: an
            // interrupt must not buy another attempt.
            if let Some(by) = *cancel.borrow() {
                break StepResult::Failed {
                    failure: StepFailure::new(
                        StepTerminalReason::Interrupted { by },
                        "interrupted before opening an attempt",
                    ),
                };
            }

            // The bound covers the Step's coding attempts. Summary calls have
            // their own bound and do not spend it.
            if ordinal >= policy.max_attempts {
                break StepResult::Failed {
                    failure: StepFailure::new(
                        StepTerminalReason::PolicyLimit {
                            policy: policy.version.clone(),
                        },
                        format!("{} physical attempts allowed", policy.max_attempts),
                    ),
                };
            }

            ordinal += 1;
            if let Some(by) = preparation::reserve(
                source,
                &mut cancel,
                &mut interrupt_available,
                policy.no_progress_timeout,
            )
            .await
            {
                break StepResult::Failed {
                    failure: StepFailure::new(
                        StepTerminalReason::Interrupted { by },
                        "interrupted while waiting for a provider attempt slot",
                    ),
                };
            }
            let preparation = match preparation::prepare(
                source,
                &mut cancel,
                &mut interrupt_available,
                step,
                ordinal,
                generation,
                policy.no_progress_timeout,
            )
            .await
            {
                Ok(prepared) => prepared,
                Err(failure) => break StepResult::Failed { failure },
            };
            let preparation = match preparation {
                Ok(prepared) => prepared,
                Err(preparation::PreparationTimeout { stage, refusal }) => {
                    // No attempt was minted. Charge a separate Step-wide bound
                    // that survives both provider retries and history rebuilds.
                    ordinal -= 1;
                    preparation_timeouts += 1;
                    grapt_diagnostics::report(format!(
                        "context preparation timeout stage={} step={step:?} retry={preparation_timeouts}",
                        stage.as_str(),
                    ));
                    if preparation_timeouts >= policy.max_attempts {
                        AttemptPreparation::refused(refusal)
                    } else {
                        let after = policy
                            .backoff_for(preparation_timeouts)
                            .min(policy.max_retry_delay);
                        if let Err(failure) = backoff::wait(
                            source,
                            &mut cancel,
                            &mut interrupt_available,
                            RetryNotice {
                                attempt: preparation_timeouts,
                                after,
                                class: None,
                                preparation_stage: Some(stage),
                                capacity_wait: None,
                            },
                        )
                        .await
                        {
                            break StepResult::Failed { failure };
                        }
                        continue;
                    }
                }
            };
            if let Some(trigger) = preparation.compaction_required() {
                // Preparation refused before mint/reserve/open, so this is not
                // a Physical Attempt and consumes no coding ordinal.
                ordinal -= 1;
                if rebuilds >= policy.max_overflow_rebuilds {
                    break StepResult::Failed {
                        failure: StepFailure::new(
                            StepTerminalReason::ContextFailure {
                                failure: ContextFailure::CompactionDidNotFit,
                            },
                            "context still exceeds admission budget after bounded compaction",
                        ),
                    };
                }
                let next = compact_and_reload(
                    history,
                    CompactionContext {
                        step,
                        preceding_ordinal: ordinal,
                        generation,
                        cancellation: cancellation.clone(),
                        trigger,
                        max_attempts: policy.max_attempts,
                    },
                )
                .await;
                match next {
                    Ok(next) => {
                        rebuilds += 1;
                        history_reloads += 1;
                        generation = next;
                        continue;
                    }
                    Err(failure) => break StepResult::Failed { failure },
                }
            }
            let progress = Arc::new(Notify::new());
            let commits = Arc::new(AtomicU64::new(0));
            let provider_finished = Arc::new(AtomicBool::new(false));

            let progress_owner = ProgressOwner {
                inner: owner,
                progress: Arc::clone(&progress),
                commits: Arc::clone(&commits),
                provider_finished: Arc::clone(&provider_finished),
            };
            let prepared = AttemptProtocol::prepare(
                preparation,
                Arc::clone(&tools),
                progress_owner,
                cancellation.residency(),
                cancel.clone(),
            );
            let (attempt, interrupt, run) = match prepared {
                Ok(PreparedAttempt {
                    attempt,
                    interrupt,
                    run,
                    ..
                }) => (attempt, interrupt, run),
                Err((returned, refusal)) => {
                    owner = returned.inner;
                    match capacity_wait
                        .refused(
                            source,
                            &mut cancel,
                            &mut interrupt_available,
                            &policy,
                            ordinal,
                            refusal,
                        )
                        .await
                    {
                        Ok(()) => {
                            ordinal -= 1;
                            continue;
                        }
                        Err(result) => break result,
                    }
                }
            };

            capacity_wait.admitted();
            let mut timed_out = false;
            let mut run = std::pin::pin!(run);
            // The deadline is pinned ONCE and reset only by real progress.
            // Recreating it on every loop turn would let an unrelated wakeup —
            // such as the cancel channel closing — silently extend the stall
            // window toward twice its length.
            let mut deadline = std::pin::pin!(tokio::time::sleep(policy.no_progress_timeout));
            let (progress_owner, report) = loop {
                let awaiting_decision = decision_waits
                    .as_ref()
                    .is_some_and(|waiting| *waiting.borrow() > 0);
                tokio::select! {
                    finished = &mut run => break finished,
                    changed = cancel.changed(), if interrupt_available => {
                        match changed {
                            Ok(()) => {
                                // Read the source into a local FIRST. An
                                // `if let` over `borrow()` keeps the watch
                                // guard alive across the `.await` below, which
                                // makes the whole driver future non-`Send`.
                                let cancelled_by = *cancel.borrow();
                                if let Some(by) = cancelled_by {
                                    interrupt.interrupt(AttemptCancelCause::Interrupted { by });
                                    break (&mut run).await;
                                }
                            }
                            // Every sender dropped: no interrupt can arrive.
                            // Only the branch is removed — the deadline keeps
                            // running from where it was.
                            Err(_) => interrupt_available = false,
                        }
                    }
                    changed = async {
                        match decision_waits.as_mut() {
                            Some(waiting) => waiting.changed().await,
                            None => std::future::pending().await,
                        }
                    }, if decision_waits.is_some() => {
                        // Resuming an effect gets a fresh stall window; elapsed
                        // human deliberation is never provider/tool silence.
                        if changed.is_err() { decision_waits = None; }
                        deadline.as_mut().reset(tokio::time::Instant::now() + policy.no_progress_timeout);
                    }
                    // Ordinary progress still requires a successful commit.
                    _ = progress.notified() => {
                        deadline
                            .as_mut()
                            .reset(tokio::time::Instant::now() + policy.no_progress_timeout);
                    }
                    _ = &mut deadline, if !awaiting_decision && !provider_finished.load(Ordering::Relaxed) => {
                        // The run future can admit a decision during this same
                        // select poll. Recheck custody before classifying silence.
                        // This watchdog ends
                        // at the provider terminal. A legal long tool poll is
                        // governed by its own yield/timeout, not stream silence.
                        if provider_finished.load(Ordering::Relaxed)
                            || decision_waits.as_ref().is_some_and(|waiting| *waiting.borrow() > 0) {
                            continue;
                        }
                        timed_out = true;
                        interrupt.interrupt(AttemptCancelCause::NoProgress);
                        break (&mut run).await;
                    }
                }
            };
            owner = progress_owner.inner;

            let decision = classify(&report, &policy, timed_out, ordinal, rebuilds);

            receipts.push(AttemptReceipt {
                attempt,
                step,
                ordinal,
                generation,
                outcome: report.outcome.clone(),
                decision: decision.clone(),
                diagnostics: report.diagnostics.clone(),
            });

            match decision {
                AttemptDecision::Settled => {
                    break match report.outcome {
                        StepOutcome::Ended { finish } => StepResult::Ended { finish },
                        // The attempt already named its own terminal reason and
                        // the Step is adopting it unchanged. Converting rather
                        // than reusing the value keeps the two vocabularies
                        // distinct types; `carried_up` is total and lossless.
                        StepOutcome::Failed { failure } => StepResult::Failed {
                            failure: StepFailure::from_attempt(
                                carried_up(failure.reason()),
                                &failure,
                            ),
                        },
                        StepOutcome::OwnerRejected { error } => StepResult::OwnerRejected { error },
                    };
                }
                AttemptDecision::Retry { after } => {
                    if let Err(failure) = backoff::wait(
                        source,
                        &mut cancel,
                        &mut interrupt_available,
                        RetryNotice {
                            attempt: ordinal,
                            after,
                            class: report
                                .provider_failure
                                .as_ref()
                                .map(|detail| detail.class.to_terminal()),
                            preparation_stage: None,
                            capacity_wait: None,
                        },
                    )
                    .await
                    {
                        break StepResult::Failed { failure };
                    }
                }
                AttemptDecision::Rebuild => {
                    // Compaction is a real operation, then a reload that yields
                    // the generation the rebuilt attempt is built from. If
                    // either fails, no further stream is opened.
                    // Compaction is its own provider call, so it is awaited —
                    // and interruptible while it runs. An interrupt here must
                    // stop before any reload or new attempt.
                    // The compaction future is created ONCE and polled to
                    // completion. Dropping it and calling compact() again would
                    // duplicate a provider call, so a closing cancel channel
                    // removes only the cancel branch.
                    let compacted = compact_and_reload(
                        history,
                        CompactionContext {
                            step,
                            preceding_ordinal: ordinal,
                            generation,
                            cancellation: cancellation.clone(),
                            trigger: grapt_types::CompactionTrigger::ContextOverflow,
                            max_attempts: policy.max_attempts,
                        },
                    )
                    .await;
                    match compacted {
                        Ok(next) => {
                            rebuilds += 1;
                            history_reloads += 1;
                            generation = next;
                        }
                        Err(failure) => break StepResult::Failed { failure },
                    }
                }
                AttemptDecision::Stop { reason } => {
                    break match report.outcome {
                        StepOutcome::OwnerRejected { error } => StepResult::OwnerRejected { error },
                        // The STEP overrode the attempt's reason: a policy bound
                        // fired, or output was already visible. The attempt's
                        // diagnostic is kept because it says what actually went
                        // wrong; the reason is now the Step's own.
                        StepOutcome::Failed { failure } => StepResult::Failed {
                            failure: StepFailure::from_attempt(reason, &failure),
                        },
                        StepOutcome::Ended { finish } => StepResult::Ended { finish },
                    };
                }
            }
        };

        source.discard_preparation();
        let result = classification::with_retry_diagnosis(result, &receipts);

        // Exactly ONE Step terminal for the whole logical Step, however many
        // attempts it took. An owner that already refused is not asked again.
        let result = match &result {
            StepResult::OwnerRejected { .. } => result,
            StepResult::Ended { finish } => {
                let finish = *finish;
                match commit_step(&mut owner, DurableFact::StepEnded { finish }) {
                    Ok(()) => result,
                    Err(error) => StepResult::OwnerRejected { error },
                }
            }
            StepResult::Failed { failure } => {
                let failure = failure.clone();
                match commit_step(&mut owner, DurableFact::StepFailed { failure }) {
                    Ok(()) => result,
                    Err(error) => StepResult::OwnerRejected { error },
                }
            }
            StepResult::AdmissionRefused { failure, .. } => {
                let failure = failure.clone();
                match commit_step(&mut owner, DurableFact::StepFailed { failure }) {
                    Ok(()) => result,
                    Err(error) => StepResult::OwnerRejected { error },
                }
            }
        };

        (
            owner,
            StepReport {
                step,
                result,
                receipts,
                history_reloads,
                compactions: rebuilds,
            },
        )
    }
}

fn commit_step<O: AttemptOwner>(owner: &mut O, fact: DurableFact) -> Result<(), OwnerError> {
    owner.commit(fact.clone())?;
    owner.notify(&fact);
    Ok(())
}

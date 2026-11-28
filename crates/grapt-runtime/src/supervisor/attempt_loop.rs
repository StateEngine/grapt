use super::task_drivers::{cancelled_tool_outcome, pump_reader, spawn_tool};
use super::*;

#[allow(clippy::too_many_arguments)]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) async fn run_attempt<F, R, T, O>(
    attempt: PhysicalAttemptId,
    make_reader: F,
    tools: Arc<T>,
    mut owner: O,
    cancel_tx: watch::Sender<Option<AttemptCancelCause>>,
    mut cancel_rx: watch::Receiver<Option<AttemptCancelCause>>,
    residency: Residency,
    step: watch::Receiver<Option<InterruptionSource>>,
) -> (O, AttemptReport)
where
    F: FnOnce() -> R + Send + 'static,
    R: ProviderReader,
    T: ToolRunner,
    O: AttemptOwner,
{
    let shared = Arc::new(SharedDiagnostics::default());
    let permits = Arc::new(Semaphore::new(ATTEMPT_TOOL_BUDGET));
    let compositions = Arc::new(Semaphore::new(nested::COMPOSITION_LIMIT));
    let mut nested_state = nested::NestedState::default();
    let (nested_tx, mut nested_rx) = mpsc::channel(nested::NESTED_CHANNEL_CAPACITY);
    let mut validator = StreamValidator::new();

    // ---- open the attempt BEFORE any provider I/O ------------------------
    //
    // Contract §"Attempt lifecycle" step 2: "append the Step/attempt-start
    // fact before issuing the provider request". If the owner refuses it there
    // is no durable attempt, so no stream may be opened and no tool may run.
    if let Err(error) = commit(
        &mut owner,
        DurableFact::AttemptStarted {
            attempt: attempt.clone(),
        },
    ) {
        return (
            owner,
            AttemptReport {
                attempt,
                outcome: StepOutcome::OwnerRejected { error },
                diagnostics: Diagnostics::default(),
                provider_failure: None,
            },
        );
    }
    if validator.accept(&AttemptFragment::AttemptOpened).is_err() {
        // Unreachable for a fresh validator; handled rather than unwrapped so
        // this function keeps its no-panic property.
        let failure = AttemptFailure::new(
            AttemptTerminalReason::ProviderFailure {
                class: TerminalClass::InvalidProviderOutput,
            },
            "attempt validator refused to open",
        );
        let _ = commit(
            &mut owner,
            DurableFact::AttemptFailed {
                failure: failure.clone(),
            },
        );
        return (
            owner,
            AttemptReport {
                attempt,
                outcome: StepOutcome::Failed { failure },
                diagnostics: Diagnostics::default(),
                provider_failure: None,
            },
        );
    }

    let (fragment_tx, mut fragment_rx) =
        mpsc::channel::<AttemptFragment>(PROVIDER_CHANNEL_CAPACITY);
    let (tool_tx, mut tool_rx) = mpsc::channel::<ToolMessage>(TOOL_CHANNEL_CAPACITY);

    // The stream opens here and only here: one factory call, after the
    // attempt-start fact is durable.
    //
    // Two things bound this task's life, and both are needed. `OwnedTask`
    // ABORTS it when this frame unwinds — see its own documentation. The
    // residency token it carries is what lets the coordinator observe the abort
    // having been HONOURED: the token dies with the task's future, which is the
    // only moment at which a replacement drain may open a reader of its own.
    let reader_residency = residency.clone();
    let reader_cancel = cancel_rx.clone();
    let reader_step = step.clone();
    let reader_shared = Arc::clone(&shared);
    let reader_task = OwnedTask(tokio::spawn(async move {
        let _residency = reader_residency;
        pump_reader(
            make_reader(),
            fragment_tx,
            reader_cancel,
            reader_step,
            reader_shared,
        )
        .await;
    }));

    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut started_tools: BTreeMap<String, ToolCallId> = BTreeMap::new();
    let mut settled_tools: BTreeMap<String, ToolOutcome> = BTreeMap::new();
    let mut hosted_calls: BTreeMap<String, ToolCallId> = BTreeMap::new();
    let mut observed_calls: BTreeMap<String, ToolCallId> = BTreeMap::new();
    let mut tools_started = 0usize;
    let mut failure: Option<AttemptFailure> = None;
    let mut finish: Option<FinishReason> = None;
    let mut cancelled: Option<AttemptCancelCause> = None;
    let mut owner_error: Option<OwnerError> = None;
    let mut provider_failure: Option<ProviderFailureDetail> = None;

    macro_rules! commit_or_stop {
        ($fact:expr) => {
            if let Err(error) = commit(&mut owner, $fact) {
                owner_error = Some(error);
                break;
            }
        };
    }

    loop {
        tokio::select! {
            biased;

            // Nested commits must acknowledge before their parent can finish.
            Some(request) = nested_rx.recv() => {
                if let Err(error) = nested_state.handle(request,&mut owner,&observed_calls) {
                    owner_error = Some(error);
                    break;
                }
            }

            // 1. Tool terminals precede new provider input.
            Some(message) = tool_rx.recv() => {
                let ToolMessage::Terminal { call, outcome } = message;
                if settled_tools.contains_key(call.as_str()) {
                    continue;
                }
                if let Err(error) = nested_state.close_parent(&mut owner,&call) {
                    owner_error = Some(error);
                    break;
                }
                settled_tools.insert(call.as_str().to_owned(), outcome.clone());
                commit_or_stop!(DurableFact::ToolTerminal { call, outcome });
            }

            // 2. Cancellation outranks new provider input.
            _ = cancel_rx.changed() => {
                if let Some(cause) = *cancel_rx.borrow() {
                    cancelled = Some(cause);
                    break;
                }
            }

            // 3. Provider input last.
            fragment = fragment_rx.recv() => {
                let Some(fragment) = fragment else {
                    // The reader can observe cancellation and close between
                    // this select's cancellation poll and its channel poll.
                    // The Step source is sampled here too: a cancel published
                    // while the reader was closing is still the attempt's
                    // cause, so the attempt settles interrupted rather than
                    // as an ordinary end.
                    cancelled = (*cancel_rx.borrow())
                        .or_else(|| step.borrow().map(|by| AttemptCancelCause::Interrupted { by }));
                    break;
                };
                match validator.accept(&fragment) {
                    Ok(accepted) => {
                        if let Some(evidence) = accepted.ignored {
                            if let Some(call) = ToolCallId::new(evidence.call.clone()) {
                                owner.observe(Observation::IgnoredStraggler {
                                    call,
                                    kind: evidence.kind,
                                });
                            }
                            continue;
                        }
                        let hosted_terminal = match &fragment {
                            AttemptFragment::HostedToolResult { call, result, .. }
                            | AttemptFragment::HostedToolFailed { call, error: result, .. } => {
                                let output = match owner.capture_hosted_output(result) {
                                    Ok(output) => output,
                                    Err(error) => { owner_error = Some(error); break; }
                                };
                                let outcome = if matches!(&fragment, AttemptFragment::HostedToolResult { .. }) {
                                    ToolOutcome::Completed { output: Some(output) }
                                } else {
                                    ToolOutcome::Failed { reason: ToolFailureReason::EffectFailed, detail: Some(output) }
                                };
                                Some((call.clone(), outcome))
                            }
                            _ => None,
                        };
                        if let AttemptFragment::ProviderTerminal { finish: reason, .. } = &fragment {
                            finish = Some(*reason);
                        }
                        if let AttemptFragment::ProviderFailure {
                            class,
                            classification,
                            diagnostic,
                            message,
                            ..
                        } = &fragment
                        {
                            // The typed class travels in the REASON now. It
                            // used to be flattened into the diagnostic as a
                            // `{class:?}` Debug string, so the one datum the
                            // driver needed was recoverable only by parsing
                            // prose.
                            failure = Some(AttemptFailure::new(
                                AttemptTerminalReason::ProviderFailure {
                                    class: class.to_terminal(),
                                },
                                message.clone(),
                            ).with_usage_diagnostic(*diagnostic).with_limit_reset(match classification {
                                Some(grapt_provider::FailureClassification::UsageLimit { reset }) => *reset,
                                _ => None,
                            }));
                            provider_failure = Some(ProviderFailureDetail {
                                class: class.clone(),
                                classification: *classification,
                            });
                        }

                        let mut call_admission_failed = false;
                        let launch = match &fragment {
                            AttemptFragment::ToolCalled { call, tool, input, execution, .. } => {
                                let key = call.as_str().to_owned();
                                if nested_state.contains(call) {
                                    call_admission_failed = true;
                                    failure = Some(AttemptFailure::new(
                                        AttemptTerminalReason::ProviderFailure { class:TerminalClass::InvalidProviderOutput },
                                        "provider call collides with a nested call identity",
                                    ));
                                }
                                if !call_admission_failed && !observed_calls.contains_key(&key) {
                                    if observed_calls.len() >= ATTEMPT_TOOL_CALL_LIMIT {
                                        call_admission_failed = true;
                                        failure = Some(AttemptFailure::new(
                                            AttemptTerminalReason::ProviderFailure {
                                                class: TerminalClass::InvalidProviderOutput,
                                            },
                                            format!(
                                                "attempt exceeded the {ATTEMPT_TOOL_CALL_LIMIT}-call tool limit"
                                            ),
                                        ));
                                    } else {
                                        observed_calls.insert(key, call.clone());
                                    }
                                }
                                if call_admission_failed {
                                    None
                                } else {
                                match execution {
                                    ToolExecution::Local => {
                                        match crate::tool_protocol::BoundedToolInput::new(
                                            input.as_bytes().to_vec(),
                                        ) {
                                            Ok(input) => Some((call.clone(), tool.clone(), input)),
                                            Err(error) => {
                                                call_admission_failed = true;
                                                failure = Some(AttemptFailure::new(
                                                    AttemptTerminalReason::ProviderFailure {
                                                        class: TerminalClass::InvalidProviderOutput,
                                                    },
                                                    error.to_string(),
                                                ));
                                                None
                                            }
                                        }
                                    }
                                    ToolExecution::ProviderHosted => {
                                        hosted_calls.insert(call.as_str().to_owned(), call.clone());
                                        None
                                    }
                                }
                                }
                            }
                            _ => None,
                        };

                        // Call count, identity and input bounds reject before
                        // durable admission. A refused call must not leave an
                        // opening without a tracked tool to close it.
                        if call_admission_failed {
                            break;
                        }

                        let terminal = accepted.terminal;

                        // Contract step 5: a local tool task starts only after
                        // its ToolCalled fact is COMMITTED. A refusal here must
                        // therefore stop before the tool is launched.
                        commit_or_stop!(DurableFact::Fragment { fragment: Box::new(fragment) });

                        // A hosted result is not settled merely because its
                        // provider fragment arrived. The fragment becomes
                        // durable first; then the closed terminal becomes
                        // durable; only then may the in-memory duplicate map
                        // say this call is settled.
                        if let Some((call, outcome)) = hosted_terminal {
                            let key = call.as_str().to_owned();
                            if let std::collections::btree_map::Entry::Vacant(entry) =
                                settled_tools.entry(key)
                            {
                                commit_or_stop!(DurableFact::ToolTerminal {
                                    call,
                                    outcome: outcome.clone(),
                                });
                                entry.insert(outcome);
                            }
                        }

                        if let Some((call, tool, input)) = launch {
                            started_tools.insert(call.as_str().to_owned(), call.clone());
                            tools_started += 1;
                            let nested = if tools.is_composition(&tool) {
                                nested_state.register(call.clone());
                                Some(NestedTools::new(call.clone(),attempt.clone(),tools.clone(),nested_tx.clone(),
                                    Arc::clone(&permits),Arc::clone(&compositions)))
                            } else { None };
                            spawn_tool(
                                &mut tasks,
                                Arc::clone(&tools),
                                Arc::clone(&permits),
                                Arc::clone(&shared),
                                tool_tx.clone(),
                                cancel_rx.clone(),
                                call,
                                tool,
                                input,
                                residency.clone(),
                                Some(attempt.clone()),
                                nested,
                            );
                        }

                        if terminal {
                            break;
                        }
                    }
                    Err(violation) => {
                        failure = Some(violation.attempt_failure());
                        break;
                    }
                }
            }

            else => break,
        }
    }

    // Classify the provider exit before any join can wait on tool approval.
    // Preserve an explicit failure's typed reason and diagnostic; normalization
    // fills only the terminal-shaped failure and missing-terminal cases.
    let mut provider_ending = match failure {
        Some(failure) => Err(failure),
        None => match finish {
            Some(finish) => match finish.normalize() {
                NormalizedFinish::Completed { .. } => Ok(finish),
                NormalizedFinish::Failed { class } => Err(AttemptFailure::new(
                    AttemptTerminalReason::ProviderFailure { class },
                    format!("provider finish {finish:?} is a failure wearing a terminal's shape"),
                )),
            },
            None => Err(AttemptFailure::new(
                AttemptTerminalReason::ProviderFailure {
                    class: TerminalClass::InvalidProviderOutput,
                },
                "reader completed with no ProviderTerminal or ProviderFailure",
            )),
        },
    };
    let refused = owner_error.is_some();

    // Abort before the reader join yields: a failed attempt must not run an
    // effect if approval arrives during cleanup. Do not publish an interrupt
    // or watchdog cause for a provider failure.
    // Every fragment already queued after a clean terminal is a violation.
    // Detect it synchronously; the later drain retains the exact count.
    let queued_violation = provider_ending.is_ok() && !fragment_rx.is_empty();
    if cancelled.is_some() || refused || provider_ending.is_err() || queued_violation {
        nested_state.stop_admission();
        tasks.abort_all();
    }

    if let Some(cause) = cancelled {
        let _ = cancel_tx.send(Some(cause));
    }
    let timing = owner.timing_context();
    {
        let _join = timing
            .as_ref()
            .map(|context| context.span(grapt_types::timing::TimingStage::ReaderJoin));
        reader_task.settle().await;
    }
    // Drain what is left in the provider channel. What that MEANS depends on
    // why the loop ended:
    //
    // - refused: the owner is not accepting anything, so the queue is drained
    //   silently. Observing would be a further notification after a refusal,
    //   and calling it post-terminal output would blame the provider for the
    //   store's refusal.
    // - cancelled: the fragments are casualties of the interrupt, explicitly
    //   classified rather than silently dropped.
    // - clean terminal: a fragment after the terminal violates the contract's
    //   "no fragment follows either terminal fragment". Draining it as a
    //   discard would evade the validator's own law, so it fails the step.
    let mut discarded = 0usize;
    let mut post_terminal = 0usize;
    while let Ok(fragment) = fragment_rx.try_recv() {
        if refused {
            drop(fragment);
        } else if cancelled.is_some() {
            discarded += 1;
            owner.observe(Observation::DiscardedOnCancel {
                fragment: Box::new(fragment),
            });
        } else {
            post_terminal += 1;
        }
    }
    if post_terminal > 0 && provider_ending.is_ok() {
        provider_ending = Err(AttemptFailure::new(
            AttemptTerminalReason::ProviderFailure {
                class: TerminalClass::InvalidProviderOutput,
            },
            format!("{post_terminal} fragment(s) arrived after the provider terminal"),
        ));
        nested_state.stop_admission();
        tasks.abort_all();
    }

    let tool_drain = timing
        .as_ref()
        .map(|context| context.span(grapt_types::timing::TimingStage::ToolDrain));

    // Join every owned tool while STILL draining tool terminals and STILL
    // watching for an interrupt. All three must happen together:
    //
    // - draining terminals during the join is what lets a refusal discovered
    //   HERE abort the remaining tools. Joining first and draining afterwards
    //   deadlocks the moment one tool outlives the others, because the refusal
    //   is not seen until the join it is supposed to end has already finished.
    // - watching cancellation keeps a late interrupt from being missed.
    let mut refused = refused;
    loop {
        tokio::select! {
            biased;

            Some(request) = nested_rx.recv(), if !refused => {
                if let Err(error) = nested_state.handle(request,&mut owner,&observed_calls) {
                    owner_error = Some(error);
                    refused = true;
                    nested_state.stop_admission();
                    tasks.abort_all();
                }
            }

            Some(ToolMessage::Terminal { call, outcome }) = tool_rx.recv(), if !refused => {
                if settled_tools.contains_key(call.as_str()) {
                    continue;
                }
                if let Err(error) = nested_state.close_parent(&mut owner,&call) {
                    owner_error = Some(error);
                    refused = true;
                    nested_state.stop_admission();
                    tasks.abort_all();
                    continue;
                }
                settled_tools.insert(call.as_str().to_owned(), outcome.clone());
                if let Err(error) = commit(&mut owner, DurableFact::ToolTerminal { call, outcome }) {
                    owner_error = Some(error);
                    refused = true;
                    tasks.abort_all();
                }
            }

            _ = cancel_rx.changed(), if cancelled.is_none() && !refused && provider_ending.is_ok() => {
                if let Some(cause) = *cancel_rx.borrow() {
                    cancelled = Some(cause);
                    nested_state.stop_admission();
                    tasks.abort_all();
                }
            }

            joined = tasks.join_next() => {
                match joined {
                    Some(_) => continue,
                    None => break,
                }
            }
        }
    }

    let effects_settled = tokio::time::timeout(Duration::from_secs(2), tools.settle_attempt())
        .await
        .unwrap_or(false);
    nested_state.stop_admission();
    while let Ok(request) = nested_rx.try_recv() {
        if !refused {
            if let Err(error) = nested_state.handle(request, &mut owner, &observed_calls) {
                owner_error = Some(error);
                refused = true;
            }
        }
    }
    if !refused {
        if let Err(error) = nested_state.close_all(&mut owner) {
            owner_error = Some(error);
            refused = true;
        }
    }

    // Anything that landed after the last task joined.
    while let Ok(ToolMessage::Terminal { call, outcome }) = tool_rx.try_recv() {
        if settled_tools.contains_key(call.as_str()) {
            continue;
        }
        settled_tools.insert(call.as_str().to_owned(), outcome.clone());
        if !refused {
            if let Err(error) = commit(&mut owner, DurableFact::ToolTerminal { call, outcome }) {
                owner_error = Some(error);
                refused = true;
            }
        }
    }

    // Classify anything that produced no terminal message as aborted, never as
    // success.
    for (key, call) in &started_tools {
        if settled_tools.contains_key(key.as_str()) {
            continue;
        }
        let outcome = if !effects_settled {
            ToolOutcome::Failed {
                reason: ToolFailureReason::EffectUnknown,
                detail: owner
                    .capture_hosted_output(
                        "tool execution did not settle before attempt ended; external effect state is unknown",
                    )
                    .ok(),
            }
        } else {
            match cancelled {
                Some(AttemptCancelCause::Interrupted { by }) => cancelled_tool_outcome(by),
                Some(AttemptCancelCause::NoProgress) => {
                    cancelled_tool_outcome(InterruptionSource::ManagedShutdown)
                }
                None => ToolOutcome::Failed {
                    reason: ToolFailureReason::EffectFailed,
                    detail: owner
                        .capture_hosted_output(
                            "tool execution ended without producing a terminal result",
                        )
                        .ok(),
                },
            }
        };
        settled_tools.insert(key.clone(), outcome.clone());
        if !refused {
            if let Err(error) = commit(
                &mut owner,
                DurableFact::ToolTerminal {
                    call: call.clone(),
                    outcome,
                },
            ) {
                owner_error = Some(error);
                refused = true;
            }
        }
    }
    for (key, call) in &hosted_calls {
        if settled_tools.contains_key(key.as_str()) {
            continue;
        }
        settled_tools.insert(key.clone(), ToolOutcome::ResultMissing);
        if !refused {
            if let Err(error) = commit(
                &mut owner,
                DurableFact::ToolTerminal {
                    call: call.clone(),
                    outcome: ToolOutcome::ResultMissing,
                },
            ) {
                owner_error = Some(error);
                refused = true;
            }
        }
    }

    drop(tool_drain);
    // Exactly one ATTEMPT terminal, after every tool terminal — unless the
    // owner is refusing facts, in which case writing one would be a lie. The
    // single Step terminal belongs to the Step driver: a Step that retries has
    // several attempts but still only one Step terminal.
    let outcome = if let Some(error) = owner_error {
        StepOutcome::OwnerRejected { error }
    } else {
        let (fact, outcome) = if let Some(cause) = cancelled {
            // THE SPLIT. A watchdog cancel is not an interruption: nobody
            // interrupted anything, the stream stalled. Settling both as
            // `aborted` — which is what the string carrier did — made a hung
            // provider indistinguishable from a user pressing stop.
            let failure = match cause {
                AttemptCancelCause::Interrupted { by } => AttemptFailure::new(
                    AttemptTerminalReason::Interrupted { by },
                    "attempt interrupted",
                ),
                AttemptCancelCause::NoProgress => AttemptFailure::new(
                    AttemptTerminalReason::ProviderFailure {
                        class: TerminalClass::NoProgressTimeout,
                    },
                    "no progress within the step's no-progress timeout",
                ),
            };
            (
                DurableFact::AttemptFailed {
                    failure: failure.clone(),
                },
                StepOutcome::Failed { failure },
            )
        } else {
            match provider_ending {
                Err(failure) => (
                    DurableFact::AttemptFailed {
                        failure: failure.clone(),
                    },
                    StepOutcome::Failed { failure },
                ),
                Ok(finish) => (
                    DurableFact::AttemptEnded { finish },
                    StepOutcome::Ended { finish },
                ),
            }
        };
        match commit(&mut owner, fact) {
            Ok(()) => outcome,
            Err(error) => StepOutcome::OwnerRejected { error },
        }
    };

    let diagnostics = Diagnostics {
        provider_high_water: shared.provider_high_water.load(Ordering::Relaxed),
        provider_contention_events: shared.provider_contention_events.load(Ordering::Relaxed),
        provider_fragments_coalesced: shared.provider_fragments_coalesced.load(Ordering::Relaxed),
        producer_wait: Duration::from_nanos(shared.producer_wait_nanos.load(Ordering::Relaxed)),
        tools_started,
        tools_peak_concurrent: shared.tools_peak_concurrent.load(Ordering::Relaxed),
        tool_permit_waits: shared.tool_permit_waits.load(Ordering::Relaxed),
        discarded_on_cancel: discarded,
        post_terminal_fragments: post_terminal,
        visible_output: validator.visible_output() || owner.committed_output(),
        cancelled: cancelled.is_some(),
    };

    (
        owner,
        AttemptReport {
            attempt,
            outcome,
            diagnostics,
            provider_failure,
        },
    )
}

#[cfg(test)]
mod tests;

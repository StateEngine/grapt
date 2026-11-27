use super::*;

impl Fixture {
    pub(super) fn record_owned_exchange_call(
        &self,
        (include_terminal, failed_before_permission, interrupted): (bool, bool, bool),
        tool: &str,
        action: &str,
        output: &[u8],
        arguments: &str,
        execution: ToolExecution,
    ) -> Result<Option<grapt_types::ArtifactRef>, String> {
        promote_test_input(self.inbox()?, &self.session, &self.input, "use the tool")?;
        let call = ToolCallId::new("call_d2a_read")
            .ok_or_else(|| "call identity was refused".to_owned())?;
        let attempt = PhysicalAttemptId::new("pat_d2a_read")
            .ok_or_else(|| "attempt identity was refused".to_owned())?;
        let step = StepNumber::new(1).ok_or_else(|| "step was refused".to_owned())?;
        let owner_artifacts = open_artifacts(&self.artifact_root)?;
        let mut owner = DurableAttemptOwner::elect_inbox_owned(
            Arc::clone(self.inbox()?),
            owner_artifacts,
            Arc::new(FixedClock),
            Arc::new(CountingIds::default()),
            NoLiveSubscribers,
            self.session.clone(),
            step,
            None,
        )
        .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::AttemptStarted {
                attempt: attempt.clone(),
            })
            .map_err(|error| error.reason)?;
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(AttemptFragment::ToolCalled {
                    call: call.clone(),
                    tool: tool.to_owned(),
                    input: arguments.to_owned(),
                    execution,
                    state: AdapterState::none(),
                }),
            })
            .map_err(|error| error.reason)?;
        if failed_before_permission {
            owner
                .commit(DurableFact::ToolTerminal {
                    call,
                    outcome: ToolTerminalOutcome::Failed {
                        reason: grapt_types::ToolFailureReason::EffectFailed,
                        detail: None,
                    },
                })
                .map_err(|error| error.reason)?;
            owner
                .commit(DurableFact::StepEnded {
                    finish: FinishReason::ToolCalls,
                })
                .map_err(|error| error.reason)?;
            return Ok(None);
        }
        if execution == ToolExecution::Local {
            self.ask_permission_for("evt_d2a_permission", "per_d2a_permission", &call, action)?;
        }

        if !include_terminal {
            if interrupted {
                owner
                    .commit(DurableFact::StepFailed {
                        failure: grapt_runtime::StepFailure::new(
                            grapt_types::StepTerminalReason::Interrupted {
                                by: grapt_types::InterruptionSource::User,
                            },
                            "fixture lost tool outcome",
                        ),
                    })
                    .map_err(|error| error.reason)?;
            }
            return Ok(None);
        }

        let bound = CaptureBound::new(MAX_BLOCK_CAPTURE_BYTES, 64 * 1_024)
            .ok_or_else(|| "artifact bound was refused".to_owned())?;
        let captured = self
            .artifacts()?
            .capture(
                Cursor::new(output),
                bound,
                ArtifactKind::CapturedOutput,
                Sensitivity::Ordinary,
            )
            .map_err(|error| error.to_string())?;
        let output_ref = captured.reference().clone();
        let output = ToolTerminalArtifact::new(output_ref.clone(), captured.disposition())
            .ok_or_else(|| "terminal artifact was refused".to_owned())?;
        owner
            .commit(DurableFact::ToolTerminal {
                call,
                outcome: ToolTerminalOutcome::Completed {
                    output: Some(output),
                },
            })
            .map_err(|error| error.reason)?;
        owner
            .commit(DurableFact::StepEnded {
                finish: FinishReason::ToolCalls,
            })
            .map_err(|error| error.reason)?;
        Ok(Some(output_ref))
    }
}

//! A call lost when its Step ended says whether its effect can have
//! happened: only an ask with no granting reply proves it never ran.
use grapt_permission::{DecisionIds, PermissionHttpReply, PermissionReplyCommand};
use grapt_types::{CasOutcome, DecisionNonce, RequestId};

use super::*;

impl Fixture {
    /// Answer the fixture's permission ask with `reply`.
    fn reply_permission(&self, reply: PermissionHttpReply) -> Result<(), String> {
        let permission = "per_d2a_permission"
            .parse::<PermissionId>()
            .map_err(|error| error.to_string())?;
        let client = AuthenticatedClientId::new("client_d2a_continuation")
            .ok_or_else(|| "client identity was refused".to_owned())?;
        let project = ProjectId::new("prj_d2a_continuation")
            .ok_or_else(|| "project identity was refused".to_owned())?;
        let decision = DecisionIds {
            event: "evt_d2a_reply"
                .parse::<EventId>()
                .map_err(|error| error.to_string())?,
            request: RequestId::new("req_d2a_reply".to_owned())
                .ok_or_else(|| "reply request identity was refused".to_owned())?,
            nonce: DecisionNonce::new("nonce_d2a_reply".to_owned())
                .ok_or_else(|| "reply nonce was refused".to_owned())?,
        };
        let mut state = PermissionHttpState::default();
        let mut inbox = lock(self.inbox()?);
        let effect = state
            .reply(
                inbox.store_mut(),
                PermissionReplyCommand {
                    owner: PermissionOwner {
                        subject: "human-d2a-continuation",
                        client: &client,
                    },
                    session: &self.session,
                    id: &permission,
                    reply,
                    project: Some(&project),
                    created_ms: 1_788_220_800_003,
                    decision,
                },
                || {
                    Err(grapt_permission::PermissionHttpError::Identity(
                        "the fixture has one ask, so no sibling cascades".to_owned(),
                    ))
                },
            )
            .map_err(|error| error.to_string())?;
        if effect.outcome() != CasOutcome::Applied {
            return Err("permission fixture reply was not applied".to_owned());
        }
        Ok(())
    }

    /// A call whose Step ended without its outcome, as a crash leaves it:
    /// asked or not, and answered with `reply` if given.
    fn record_lost_call(
        &self,
        asked: bool,
        reply: Option<PermissionHttpReply>,
    ) -> Result<ToolCallId, String> {
        promote_test_input(self.inbox()?, &self.session, &self.input, "use the tool")?;
        let call = ToolCallId::new("call_d2a_read")
            .ok_or_else(|| "call identity was refused".to_owned())?;
        let attempt = PhysicalAttemptId::new("pat_d2a_read")
            .ok_or_else(|| "attempt identity was refused".to_owned())?;
        let step = StepNumber::new(1).ok_or_else(|| "step was refused".to_owned())?;
        let mut owner = DurableAttemptOwner::elect_inbox_owned(
            Arc::clone(self.inbox()?),
            open_artifacts(&self.artifact_root)?,
            Arc::new(FixedClock),
            Arc::new(CountingIds::default()),
            NoLiveSubscribers,
            self.session.clone(),
            step,
            None,
        )
        .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::AttemptStarted { attempt })
            .map_err(|error| error.reason)?;
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(AttemptFragment::ToolCalled {
                    call: call.clone(),
                    tool: READ_TOOL.to_owned(),
                    input: r#"{"path":"fixture.txt"}"#.to_owned(),
                    execution: ToolExecution::Local,
                    state: AdapterState::none(),
                }),
            })
            .map_err(|error| error.reason)?;
        if asked {
            self.ask_permission("evt_d2a_permission", "per_d2a_permission", &call)?;
        }
        if let Some(reply) = reply {
            self.reply_permission(reply)?;
        }
        owner
            .commit(DurableFact::StepFailed {
                failure: grapt_runtime::StepFailure::new(
                    grapt_types::StepTerminalReason::DurabilityFailure,
                    "fixture process lost its tool outcome",
                ),
            })
            .map_err(|error| error.reason)?;
        Ok(call)
    }
}

#[test]
fn an_unsettled_call_says_whether_its_effect_can_have_happened() -> Result<(), String> {
    // (asked, reply) -> whether the effect provably never ran.
    let cases = [
        // Auto-allowed by a rule: no ask, so the log cannot tell.
        (false, None, false),
        // Asked and never answered: the effect cannot have started.
        (true, None, true),
        // Granted: the effect may have started before the Step ended.
        (true, Some(PermissionHttpReply::Once), false),
        (true, Some(PermissionHttpReply::Always), false),
        // Rejected: the effect was refused.
        (true, Some(PermissionHttpReply::Reject), true),
    ];
    for (index, (asked, reply, never_ran)) in cases.into_iter().enumerate() {
        let fixture = Fixture::new(&format!("lost-call-{index}"))?;
        let call = fixture.record_lost_call(asked, reply)?;
        let projection = tool_projection(
            fixture.inbox()?,
            fixture.artifacts()?,
            &fixture.session,
            &call,
        )?;
        assert_eq!(projection.never_ran, never_ran, "{asked} {reply:?}");
        let state: serde_json::Value =
            serde_json::from_str(&crate::dogfood::history_json::tool_state_json(
                &projection,
                fixture.artifacts()?,
                true,
            )?)
            .map_err(|error| error.to_string())?;
        assert_eq!(state["status"], "error", "{state}");
        assert_eq!(state["error"]["type"], "ToolOutcomeUnavailable", "{state}");
        let message = state["error"]["message"].as_str().unwrap_or_default();
        let expected = if never_ran {
            "nothing was changed"
        } else {
            "it may have run"
        };
        assert!(message.contains(expected), "{asked} {reply:?}: {message}");
        assert!(message.contains("a message to continue"), "{message}");
    }
    Ok(())
}

use super::*;
use crate::backend::CommandId;

#[derive(Debug)]
struct SidePeer {
    inner: ContractDouble,
    requests: Vec<serde_json::Value>,
    reply: Option<WireResponse>,
    interrupts: usize,
    canceled: bool,
}
impl Transport for SidePeer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        assert_ne!(
            request.operation(),
            Operation::SideQuestion,
            "side request blocked the event driver"
        );
        if request.operation() == Operation::SessionInterrupt {
            self.interrupts += 1;
        }
        self.inner.call(request)
    }
}
impl Connection for SidePeer {
    fn begin_operator(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        assert_eq!(request.operation(), Operation::SideQuestion);
        self.requests.push(
            serde_json::from_slice(request.body().ok_or(TransportError::NotConnected)?)
                .map_err(|_| TransportError::NotConnected)?,
        );
        Ok(None)
    }
    fn poll_operator(&mut self) -> Result<Option<WireResponse>, TransportError> {
        Ok(self.reply.take())
    }
    fn cancel_operator(&mut self) {
        self.canceled = true;
        self.reply = None;
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
}

/// The completion an owner settled, without the command it answers.
fn completion(
    submissions: &mut crate::backend::first_prompt::Submissions,
) -> Option<crate::backend::commands::Completion> {
    submissions.command.take().map(|(_, done)| done)
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
fn side_question_keeps_controls_live_and_discards_foreign_or_failed_answers() -> TestResult {
    let mut client = Client::new(
        SidePeer {
            inner: ContractDouble::new(),
            requests: Vec::new(),
            reply: None,
            interrupts: 0,
            canceled: false,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project")?;
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client
        .models()
        .iter()
        .find(|model| model.enabled)
        .cloned()
        .ok_or("model")?;
    client.select_model(&model)?;
    client.create_session()?;
    let parent = client.selected().cloned().ok_or("parent")?;
    let invalid = execute(&mut client, "/side").ok_or("side command")?;
    assert!(!invalid.consume);
    assert!(invalid
        .notice
        .starts_with("Side question failed: Use /side "));
    assert!(client.peer_mut().requests.is_empty());
    let mut submissions = crate::backend::first_prompt::Submissions::default();
    submissions.submit(&mut client, "/side why?".into(), false, CommandId::TEST);
    assert!(client.side_question_pending());
    assert_eq!(
        submissions.outcome.take().map(|(_, outcome)| outcome),
        Some(("/side why?".into(), false, false)),
        "pending generation must release composer submission without consuming its draft"
    );
    assert!(submissions.advance(&mut client).is_none());
    client.interrupt()?;
    assert_eq!(client.peer_mut().interrupts, 1);
    assert!(
        client.begin_mcp_sign_in("docs").is_err(),
        "sign-in replaced the pending side request"
    );
    client.peer_mut().reply = Some(WireResponse {
        status: 200,
        body: br#"{"text":"side answer"}"#.to_vec(),
    });
    assert!(submissions.advance(&mut client).is_some());
    let done = completion(&mut submissions).ok_or("side completion")?;
    assert!(done.consume);
    assert!(
        matches!(done.view, Some(View::Text(lines)) if lines.iter().any(|line| line == "side answer"))
    );
    assert_eq!(client.selected(), Some(&parent));
    assert_eq!(
        client.peer_mut().requests,
        [serde_json::json!({"prompt":"why?"})]
    );

    submissions.submit(&mut client, "/side refused".into(), false, CommandId::TEST);
    client.peer_mut().reply = Some(WireResponse {
        status: 503,
        body: Vec::new(),
    });
    assert!(submissions.advance(&mut client).is_some());
    let failed = completion(&mut submissions).ok_or("failed completion")?;
    assert!(!failed.consume);
    assert!(matches!(failed.view, Some(View::Notice(message)) if message.contains("503")));
    assert!(submissions.advance(&mut client).is_none());
    assert_eq!(
        client.peer_mut().requests.len(),
        2,
        "failed question was retried"
    );

    client.peer_mut().canceled = false;
    submissions.submit(
        &mut client,
        "/side old Session".into(),
        false,
        CommandId::TEST,
    );
    client.create_session()?;
    assert!(!client.side_question_pending());
    assert!(client.peer_mut().canceled);
    assert!(submissions.advance(&mut client).is_some());
    let changed = completion(&mut submissions).ok_or("changed completion")?;
    assert!(!changed.consume && changed.view.is_none());
    assert_eq!(client.peer_mut().requests.len(), 3);
    Ok(())
}

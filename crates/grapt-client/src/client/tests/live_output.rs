//! Re-reading history never contributes to observed live output. An install
//! that finds the execution running while the estimate is unknown (after a
//! cut) starts a new zero baseline: it counts only bytes since recovery.
use super::*;

#[derive(Debug)]
struct ProgressPeer {
    inner: ContractDouble,
    frames: std::collections::VecDeque<Result<FramePoll, TransportError>>,
    /// Whether this stream has ended, cut or refused.
    ended: bool,
}
impl Transport for ProgressPeer {
    fn call(&mut self, request: &WireRequest) -> Result<crate::WireResponse, TransportError> {
        self.inner.call(request)
    }
}
impl Connection for ProgressPeer {
    fn subscribed(&self) -> bool {
        !self.ended
    }
    fn resubscribe(&mut self) {
        self.ended = false;
        self.inner.resubscribe();
    }
    /// The double's own stream, with one injected defect served first once
    /// the stream has opened.
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        if let Some(connect) = self.inner.connect() {
            return Ok(connect);
        }
        match self.frames.pop_front() {
            Some(frame) => frame.map(|frame| {
                self.ended = matches!(frame, FramePoll::Closed | FramePoll::Gap);
                self.inner.serve(frame)
            }),
            None => self.inner.poll_frame(),
        }
    }
}

/// A client whose selected session holds live output, ready for one defect.
fn live_output_client() -> Result<(Client<ProgressPeer>, SessionWireId), Box<dyn std::error::Error>>
{
    let mut client = Client::new(
        ProgressPeer {
            inner: ContractDouble::new(),
            frames: Default::default(),
            ended: false,
        },
        Bounds::PRODUCTION,
    );
    client.select_first_offer()?;
    let session = client.create_session()?;
    prove(&mut client);
    client
        .connection
        .inner
        .report_running(&session, "exe_progress_pump");
    client.reconcile(&session)?;
    // Nine bytes of prose already streamed under the running execution.
    client
        .views
        .get_mut(&session)
        .ok_or("live view")?
        .streamed_text_bytes = Some(9);
    Ok((client, session))
}

/// The frame or transport fault one defect injects into the pump.
fn defect_frame(
    client: &mut Client<ProgressPeer>,
    defect: &str,
    session: &SessionWireId,
) -> Result<Result<FramePoll, TransportError>, Box<dyn std::error::Error>> {
    let member = if defect == "gap" {
        crate::contract::EventMember::ExecutionSucceeded
    } else {
        crate::contract::EventMember::TextEnded
    };
    let mut frame = serde_json::json!({
        "id": "evt_progress_pump", "type": member.as_str(),
        "data": {"sessionID": session, "executionID": "exe_progress_pump"},
    });
    if member.requires_created() {
        frame["created"] = 1u64.into();
    }
    if member.requires_durable() {
        frame["durable"] = serde_json::json!({"aggregateID": session,"seq":1,"version":1});
    }
    if defect == "gap" {
        client.cursor = DurableCursor::new();
        client
            .cursor
            .adopt_baseline(&crate::wire::session_id("ses_other_baseline")?, 0);
    }
    Ok(match defect {
        "oversized" => Ok(FramePoll::Frame(
            "x".repeat(client.bounds.max_frame_bytes + 1),
        )),
        "closed" => Ok(FramePoll::Closed),
        "overflow" => Ok(FramePoll::Gap),
        "malformed" => Ok(FramePoll::Frame("{not json".into())),
        "transport" => Err(TransportError::FrameTooLarge {
            bound: client.bounds.max_frame_bytes,
        }),
        _ => Ok(FramePoll::Frame(frame.to_string())),
    })
}

#[test]
fn live_output_pump_recovery_invalidates_loss_but_not_an_ordinary_refresh(
) -> Result<(), Box<dyn std::error::Error>> {
    for defect in [
        "oversized",
        "overflow",
        "projection",
        "gap",
        "closed",
        "malformed",
        "transport",
    ] {
        let (mut client, session) = live_output_client()?;
        assert_eq!(
            client.view().and_then(SessionView::streamed_text_bytes),
            Some(9)
        );
        let incoming = defect_frame(&mut client, defect, &session)?;
        client.connection.frames.push_back(incoming);
        let result = client.pump();
        if defect == "transport" {
            assert!(result.is_err());
        } else {
            result?;
        }
        if defect == "gap" {
            assert_eq!(client.diagnostics().gaps_observed, 1);
        }
        if defect == "projection" {
            assert_eq!(client.diagnostics().frames_applied, 1);
        }
        // Only the paths that refuse live data on behalf of the projection are
        // gaps. An envelope the frozen contract refuses is dropped without a
        // cursor decision, so it is deliberately not one of them.
        let observed_gap = matches!(defect, "overflow" | "gap");
        if observed_gap {
            assert!(
                client.diagnostics().gaps_observed >= 1,
                "{defect}: the refused frame must be counted as a gap"
            );
        }
        // The raise itself is proven between refusal and install in
        // `a_refusal_raises_gap_recovery_and_a_baseline_ends_it` below: this
        // double answers the history read inside the same pump, so by here the
        // flag is correctly already healed.
        prove(&mut client);
        client.reconcile(&session)?;
        // A re-read baseline ends the recovery wherever it started.
        assert!(
            !client.gap_recovery_pending(),
            "a re-read baseline ends the gap recovery: {defect}"
        );
        if matches!(defect, "oversized" | "overflow") {
            assert_eq!(client.diagnostics().stream_overflows, 1);
            assert!(client.stream_gap_notice().is_some());
        } else {
            assert!(client.stream_gap_notice().is_none());
        }
        assert_eq!(
            client.view().and_then(SessionView::streamed_text_bytes),
            Some(0),
            "{defect}: the lost bytes never carry; the re-read counts from zero"
        );
    }
    Ok(())
}

/// The raise and the clear, observed on either side of the install.
///
/// `live_data_refused` is the one place a refusal becomes a recovery. Its
/// proof has to bracket the install: a pump on a synchronous double refuses the
/// frame and installs the re-read baseline before returning, so deleting the
/// set would otherwise leave every assertion above green.
#[test]
fn a_refusal_raises_gap_recovery_and_a_baseline_ends_it() -> Result<(), Box<dyn std::error::Error>>
{
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    client.select_first_offer()?;
    let session = client.create_session()?;
    prove(&mut client);
    client.reconcile(&session)?;
    assert!(
        !client.gap_recovery_pending(),
        "a live baseline is not a gap"
    );

    // Refuse a live frame the way the stream does: the cursor records the gap
    // and the client asks for history. No install happens here.
    client.cursor.buffer_overflowed();
    client.live_data_refused();
    assert!(
        client.gap_recovery_pending(),
        "a refused live frame must be reported as recovering"
    );

    // The re-read baseline is the only thing that ends it.
    client.reconcile(&session)?;
    assert!(
        !client.gap_recovery_pending(),
        "installing the baseline must end the recovery"
    );
    Ok(())
}

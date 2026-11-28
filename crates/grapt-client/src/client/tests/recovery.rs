//! Retry scheduling is bounded; authorization and malformed DTOs stay fatal.
use super::*;
use std::time::Duration;

#[derive(Debug)]
struct RetryPeer {
    inner: ContractDouble,
    attempts: usize,
    response: Option<crate::transport::WireResponse>,
}
impl Transport for RetryPeer {
    fn call(
        &mut self,
        request: &WireRequest,
    ) -> Result<crate::transport::WireResponse, TransportError> {
        self.inner.call(request)
    }
}
impl Connection for RetryPeer {
    fn subscribed(&self) -> bool {
        false
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        Ok(FramePoll::Idle)
    }
    fn resubscribe(&mut self) {}
    fn begin_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        self.attempts += 1;
        Err(TransportError::Unavailable {
            reason: "fixture disconnected".into(),
        })
    }
    fn begin_read(
        &mut self,
        _: &WireRequest,
    ) -> Result<Option<crate::transport::WireResponse>, TransportError> {
        Ok(self.response.take())
    }
    fn poll_read(&mut self) -> Result<Option<crate::transport::WireResponse>, TransportError> {
        Err(TransportError::Unavailable {
            reason: "fixture timed out".into(),
        })
    }
}
fn retry_client() -> Client<RetryPeer> {
    Client::new(
        RetryPeer {
            inner: ContractDouble::new(),
            attempts: 0,
            response: None,
        },
        Bounds::PRODUCTION,
    )
}

#[test]
fn failed_reconnect_waits_and_caps_backoff() -> Result<(), ClientError> {
    let mut client = retry_client();
    client.pump_at(Duration::ZERO)?;
    client.pump_at(Duration::from_millis(249))?;
    assert_eq!(
        client.peer_mut().attempts,
        1,
        "input polls must not launch a retry storm"
    );
    client.pump_at(Duration::from_millis(250))?;
    assert_eq!(client.peer_mut().attempts, 2);
    for seconds in [1, 2, 4, 8, 12, 16] {
        client.pump_at(Duration::from_secs(seconds))?;
    }
    let attempts = client.peer_mut().attempts;
    client.pump_at(Duration::from_millis(19_999))?;
    assert_eq!(client.peer_mut().attempts, attempts);
    client.pump_at(Duration::from_secs(20))?;
    assert_eq!(client.peer_mut().attempts, attempts + 1);
    assert_eq!(client.connection_status(), ConnectionStatus::Offline);
    Ok(())
}

#[test]
fn a_history_timeout_preserves_the_app_but_auth_and_bad_dtos_do_not() -> Result<(), ClientError> {
    let mut client = retry_client();
    client.selected = Some(crate::wire::session_id("ses_000000000001")?);
    client.pump_at(Duration::ZERO)?;
    client.pump_at(Duration::from_millis(50))?;
    assert!(
        client.recovery.history.is_none(),
        "failed partial history must not be adopted"
    );
    assert!(!client.stream_state().is_live());
    for (status, body) in [(401, Vec::new()), (200, b"not a message page".to_vec())] {
        let mut client = retry_client();
        client.selected = Some(crate::wire::session_id("ses_000000000001")?);
        client.peer_mut().response = Some(crate::transport::WireResponse { status, body });
        assert!(
            client.pump_at(Duration::ZERO).is_err(),
            "{status} must stay fatal"
        );
    }
    Ok(())
}

#[derive(Debug)]
struct QuietPeer {
    inner: ContractDouble,
    fail_history: bool,
    after_send: bool,
    attempts: usize,
}

impl Transport for QuietPeer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if matches!(
            request.operation(),
            Operation::MessageList | Operation::SessionSnapshot
        ) {
            self.attempts += 1;
            if std::mem::take(&mut self.fail_history) {
                if self.after_send {
                    return Err(TransportError::AfterSend {
                        reason: "read head: timed out".into(),
                    });
                }
                return Ok(WireResponse {
                    status: 503,
                    body: Vec::new(),
                });
            }
        }
        self.inner.call(request)
    }
}

impl Connection for QuietPeer {
    fn subscribed(&self) -> bool {
        true
    }
    fn resubscribe(&mut self) {}
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
}

#[test]
fn quiet_open_subscription_wakes_for_failed_history_then_returns_to_timerless_idle(
) -> Result<(), ClientError> {
    for after_send in [false, true] {
        let mut inner = ContractDouble::new();
        let session = seed(&mut inner, "quiet");
        let mut client = Client::new(
            QuietPeer {
                inner,
                fail_history: false,
                after_send,
                attempts: 0,
            },
            Bounds::PRODUCTION,
        );
        client.select(&session)?;
        // The snapshot waits for the stream's first frame, read at the poll.
        assert_eq!(
            client.recovery_wait_at(Duration::ZERO),
            Some(Duration::from_millis(10))
        );
        client.pump_at(Duration::ZERO)?;
        assert_eq!(client.recovery_wait_at(Duration::ZERO), None);
        client.peer_mut().fail_history = true;
        assert!(client.stream_state().is_live());
        client
            .peer_mut()
            .inner
            .run_turn(&session, "refresh this history", "recovered reply");
        client.pump_at(Duration::ZERO)?;
        let attempts = client.peer_mut().attempts;
        assert!(
            client.peer_mut().subscribed(),
            "no socket closure supplies another wake"
        );
        let delay = client.recovery_wait_at(Duration::ZERO).unwrap_or_else(|| {
            panic!("history retry must schedule a wake on the open subscription")
        });
        assert_eq!(delay, Duration::from_millis(250));
        client.pump_at(delay.saturating_sub(Duration::from_millis(1)))?;
        assert_eq!(client.peer_mut().attempts, attempts);
        client.pump_at(delay)?;
        assert!(client.peer_mut().attempts > attempts);
        assert!(client.stream_state().is_live());
        assert_eq!(
            client.recovery_wait_at(delay),
            None,
            "no periodic healthy-idle polls"
        );
        assert!(client.view().is_some_and(|view| view
            .slices
            .iter()
            .any(|slice| slice.text.contains("recovered reply"))));
    }
    Ok(())
}

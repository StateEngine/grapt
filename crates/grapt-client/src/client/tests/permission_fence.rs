//! A permission list read while a reply is acknowledged is never applied over
//! the reply.
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The contract double, with permission list reads held until released.
#[derive(Debug)]
struct Held {
    inner: ContractDouble,
    held: Option<WireResponse>,
    release: bool,
}
impl Transport for Held {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        self.inner.call(request)
    }
}
impl Connection for Held {
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        let response = self.inner.call(request)?;
        if request.operation() == Operation::PermissionRequestList {
            self.held = Some(response);
            return Ok(None);
        }
        Ok(Some(response))
    }
    fn poll_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        Ok(if self.release { self.held.take() } else { None })
    }
    fn cancel_read(&mut self) {
        self.held = None;
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

#[test]
fn a_repair_list_read_across_a_reply_is_left_to_the_fenced_lane() -> TestResult {
    let mut client = Client::new(
        Held {
            inner: ContractDouble::new(),
            held: None,
            release: false,
        },
        Bounds::PRODUCTION,
    );
    client.select_first_offer()?;
    let session = client.create_session()?;
    let ask = seed_ask(&mut client.connection.inner, &session, "write a file");
    client.request_history();
    client.pump()?;
    assert!(client.connection.held.is_some(), "the list read is held");
    client.send_permission_reply(&session, &ask, crate::wire::PermissionReplyChoice::Once)?;
    client.connection.release = true;
    client.pump()?;
    assert!(
        client
            .permissions()
            .iter()
            .all(|open| open.request_id != ask),
        "the held list brought the answered ask back"
    );
    assert!(
        client.take_permission_refresh(),
        "no fenced re-read was asked for"
    );
    Ok(())
}

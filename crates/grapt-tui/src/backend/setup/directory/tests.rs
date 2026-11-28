use super::*;
use grapt_client::{
    Bounds, ContractDouble, FramePoll, Operation, Transport, TransportError, WireRequest,
    WireResponse,
};
#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    pending: bool,
    fail: bool,
    writes: usize,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if matches!(
            request.operation(),
            Operation::SessionCreate | Operation::SessionPrompt
        ) {
            self.writes += 1;
        }
        self.inner.call(request)
    }
}
impl Connection for Peer {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        if request.operation() != Operation::ProjectCurrent {
            return self.call(request).map(Some);
        }
        self.pending = true;
        Ok(None)
    }
    fn cancel_read(&mut self) {
        self.pending = false;
    }
    fn poll_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        if self.pending {
            self.pending = false;
            return Ok(None);
        }
        if self.fail {
            return Ok(Some(WireResponse {
                status: 400,
                body: grapt_client::wire::encode_invalid_request("dubious ownership").map_err(
                    |error| TransportError::Io {
                        reason: error.to_string(),
                    },
                )?,
            }));
        }
        self.inner
            .call(
                &WireRequest::new(Operation::ProjectCurrent, None, None)?
                    .with_query("location[directory]", "/work/project-a"),
            )
            .map(Some)
    }
}
#[test]
fn failed_directory_keeps_unknown_binding_and_retry_restores_the_live_roster(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!("r-directory-{}", std::process::id()));
    std::fs::create_dir(&path)?;
    let checked = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut client = Client::new(
            Peer {
                inner: ContractDouble::new(),
                pending: false,
                fail: true,
                writes: 0,
            },
            Bounds::PRODUCTION,
        );
        client.set_catalog_profile(&path);
        let mut directory = Directory::new("/work/project-a".into());
        assert!(directory.advance(&mut client).is_none());
        assert!(directory.advance(&mut client).is_none());
        assert!(client.project().is_none());
        assert!(super::super::roster(&client).is_none());
        assert_eq!(directory.advance(&mut client), Some(Readiness::Unresolved));
        assert!(directory
            .notice()
            .is_some_and(|note| note.contains("dubious ownership")));
        assert!(directory.wait().is_none());
        for _ in 0..4 {
            assert!(directory.advance(&mut client).is_none());
        }
        assert!(client.project().is_none());
        assert!(client.create_session().is_err());
        let mut retained = Some(directory);
        let mut readiness = Readiness::Unresolved;
        super::super::retry(&mut client, &mut retained, &mut readiness, None, None)?;
        directory = retained.ok_or("retry lost the directory")?;
        client.peer_mut().fail = false;
        assert!(directory.advance(&mut client).is_none());
        assert!(directory.advance(&mut client).is_none());
        assert_eq!(directory.advance(&mut client), Some(Readiness::Loading));
        assert!(super::super::roster(&client).is_none());
        assert!(client.recovery_wait().is_some());
        for _ in 0..3 {
            client.pump()?;
        }
        assert!(super::super::roster(&client).is_some_and(|rows| !rows.is_empty()));
        assert_eq!(crate::launch::readiness(&client), Readiness::Manual);
        assert_eq!(client.peer_mut().writes, 0);
        Ok(())
    })();
    std::fs::remove_dir_all(path)?;
    checked
}

#[test]
fn selecting_a_session_cancels_both_delayed_directory_success_and_refusal(
) -> Result<(), Box<dyn std::error::Error>> {
    for fail in [false, true] {
        let inner = ContractDouble::new();
        let mut client = Client::new(
            Peer {
                inner,
                pending: false,
                fail,
                writes: 0,
            },
            Bounds::PRODUCTION,
        );
        client.offer_directory("/work/project-a")?;
        client.begin_directory_draft()?;
        let agent = client.agents().first().ok_or("agent")?.id.clone();
        let model = client.models().first().ok_or("model")?.clone();
        client.select_agent(&agent)?;
        client.select_model(&model)?;
        let session = client.create_session()?;
        client.peer_mut().writes = 0;
        let mut directory = Some(Directory::new("/work/old-directory".into()));
        assert!(directory
            .as_mut()
            .ok_or("directory")?
            .advance(&mut client)
            .is_none());
        assert!(client.peer_mut().pending);
        super::super::leave_directory(&mut client, &mut directory);
        client.resume_session(&session)?;
        assert!(directory.is_none());
        assert!(!client.peer_mut().pending);
        let project = client.project().cloned();
        for _ in 0..4 {
            assert!(directory
                .as_mut()
                .and_then(|directory| directory.advance(&mut client))
                .is_none());
            client.pump()?;
        }
        assert_eq!(client.selected(), Some(&session));
        assert_eq!(client.project(), project.as_ref());
        assert_eq!(client.peer_mut().writes, 0);
    }
    Ok(())
}

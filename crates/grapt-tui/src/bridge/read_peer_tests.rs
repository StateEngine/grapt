//! A periodic read sends nothing on an attachment already revoked.
use super::*;
use grapt_client::BasicCredential;
use std::net::TcpListener;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
fn a_read_on_a_refused_attachment_never_reaches_the_service() -> Result {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let rpc = HttpConnection::attach(
        listener.local_addr()?,
        BasicCredential::new("fixture", "local-only")?,
        Bounds::PRODUCTION,
    );
    let mut bridge = Bridge::test_rpc(rpc);
    bridge.fence.store(true, Ordering::Release);
    // The read was queued before the service was refused.
    let peer = bridge.read_peer()?;
    bridge.refuse_stream("session snapshots");
    let request = WireRequest::new(
        grapt_client::contract::Operation::PermissionRequestList,
        None,
        None,
    )?;
    assert!(peer.call(&request).is_err());
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "a request reached the refused service"
    );
    Ok(())
}

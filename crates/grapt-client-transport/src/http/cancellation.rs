//! Cancels a dedicated blocking event reader without polling a quiet socket.
use super::*;

#[derive(Debug)]
pub struct StreamCancellation(TcpStream);

impl Drop for StreamCancellation {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

impl HttpConnection {
    /// Dropping this handle interrupts `wait_frame`, including during teardown.
    /// The caller must keep it alive for the lifetime of the subscription.
    pub fn stream_cancellation(&self) -> Result<Option<StreamCancellation>, TransportError> {
        self.stream
            .as_ref()
            .map(|stream| {
                stream
                    .try_clone()
                    .map(StreamCancellation)
                    .map_err(|error| TransportError::from_io("clone subscription", error))
            })
            .transpose()
    }
}

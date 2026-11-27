//! Bounded HTTP request framing for the two concurrent transport slots.
use serde_json::Value;
use std::time::Duration;
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
};

pub(crate) async fn request(
    socket: &mut TcpStream,
    version: u8,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    let boundary;
    loop {
        let mut chunk = [0; 2048];
        let count = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut chunk)).await??;
        if count == 0 || bytes.len() + count > 65536 {
            return Err("request framing".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
            boundary = index + 4;
            break;
        }
    }
    let header = std::str::from_utf8(&bytes[..boundary])?;
    assert!(header.starts_with(&format!(
        "POST /remote-seat-mail-transport/v{version} HTTP/1.1\r\n"
    )));
    assert!(header
        .lines()
        .any(|s| s.eq_ignore_ascii_case("authorization: Bearer fixture-secret")));
    let length = header
        .lines()
        .find_map(|s| {
            s.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>())
        })
        .ok_or("length")??;
    if length > 32768 {
        return Err("request body bound".into());
    }
    while bytes.len() < boundary + length {
        let mut chunk = [0; 2048];
        let count = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut chunk)).await??;
        if count == 0 || bytes.len() + count > 65536 {
            return Err("request body".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(&bytes[boundary..boundary + length])?)
}

// TCP connect readiness and request framing are independent for the advisory
// and ordinary slots. Neither may block acceptance of the other connection.
type Framed = (
    tokio::net::TcpStream,
    Result<Value, Box<dyn std::error::Error>>,
);
type Framing = std::pin::Pin<Box<dyn std::future::Future<Output = Framed>>>;
pub(crate) struct FramingSlots(Vec<Framing>);
impl FramingSlots {
    pub(crate) fn new() -> Self {
        Self(Vec::new())
    }
    pub(crate) async fn next(
        &mut self,
        listener: &TcpListener,
        version: u8,
    ) -> Result<Framed, Box<dyn std::error::Error>> {
        let framing = &mut self.0;
        std::future::poll_fn(|cx| {
            for index in 0..framing.len() {
                if let std::task::Poll::Ready(value) = framing[index].as_mut().poll(cx) {
                    drop(framing.remove(index));
                    return std::task::Poll::Ready(Ok(value));
                }
            }
            if framing.len() < 2 {
                match listener.poll_accept(cx) {
                    std::task::Poll::Ready(Ok((mut socket, _))) => {
                        framing.push(Box::pin(async move {
                            let result = request(&mut socket, version).await;
                            (socket, result)
                        }));
                        cx.waker().wake_by_ref();
                    }
                    std::task::Poll::Ready(Err(error)) => {
                        return std::task::Poll::Ready(Err(error.into()))
                    }
                    std::task::Poll::Pending => {}
                }
            }
            std::task::Poll::Pending
        })
        .await
    }
}

//! Explicit HTTP body framing for the coalescing classification witness.
use super::*;

pub(super) fn respond(stream: &mut TcpStream, chunks: &[Vec<u8>]) {
    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
        .unwrap_or_else(|error| panic!("chunked response head: {error}"));
    for chunk in chunks {
        write!(stream, "{:x}\r\n", chunk.len())
            .and_then(|()| stream.write_all(chunk))
            .and_then(|()| stream.write_all(b"\r\n"))
            .and_then(|()| stream.flush())
            .unwrap_or_else(|error| panic!("chunked response body: {error}"));
    }
    stream
        .write_all(b"0\r\n\r\n")
        .and_then(|()| stream.flush())
        .unwrap_or_else(|error| panic!("chunked response end: {error}"));
}

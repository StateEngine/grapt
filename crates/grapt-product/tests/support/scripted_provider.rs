//! Bounded loopback Responses scripts for ordinary provider consumers.
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug)]
pub(crate) struct Server {
    pub(crate) address: SocketAddr,
    stopped: Arc<AtomicBool>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Server {
    pub(crate) fn start(
        scripts: Vec<String>,
        observe: impl Fn(&Value) -> bool + Send + 'static,
    ) -> Result<Self, String> {
        if scripts.len() > 64 || scripts.iter().map(String::len).sum::<usize>() > 8 * 1024 * 1024 {
            return Err("fixture response scripts exceed their bound".into());
        }
        let mut scripts = std::collections::VecDeque::from(scripts);
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let stopped = Arc::new(AtomicBool::new(false));
        let closed = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            while !closed.load(Ordering::Acquire) {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                if closed.load(Ordering::Acquire) {
                    break;
                }
                if let Err(error) = respond(stream, scripts.pop_front(), &observe) {
                    if !error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                        )
                    }) {
                        panic!("loopback fixture response failed: {error}");
                    }
                }
            }
        });
        Ok(Self {
            address,
            stopped,
            worker: Mutex::new(Some(worker)),
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _wake = TcpStream::connect_timeout(&self.address, Duration::from_secs(1));
        let worker = match self.worker.get_mut() {
            Ok(worker) => worker.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(worker) = worker {
            assert!(worker.join().is_ok(), "loopback fixture worker panicked");
        }
    }
}

fn respond(
    mut stream: TcpStream,
    script: Option<String>,
    observe: &impl Fn(&Value) -> bool,
) -> Result<(), Box<dyn std::error::Error>> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut wire = Vec::new();
    let body = loop {
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            // Cancellation can close a connected socket before the body arrives.
            return Ok(());
        }
        if wire.len() + count > 4 * 1024 * 1024 {
            return Err("provider request exceeded its bound".into());
        }
        wire.extend_from_slice(&chunk[..count]);
        if let Some(boundary) = wire.windows(4).position(|part| part == b"\r\n\r\n") {
            let head = std::str::from_utf8(&wire[..boundary])?;
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|length| length.parse::<usize>().ok())
                })
                .ok_or("provider request length absent")?;
            let start = boundary + 4;
            if wire.len() >= start + length {
                break serde_json::from_slice::<Value>(&wire[start..start + length])?;
            }
        }
    };
    if observe(&body) {
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")?;
        stream.flush()?;
        // Keep a real response open until the consumer cancels its attempt.
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut byte = [0];
        if stream.read(&mut byte)? != 0 {
            return Err("held provider response received unexpected client bytes".into());
        }
        return Ok(());
    }
    let prompt = body["input"]
        .as_array()
        .and_then(|input| input.iter().rev().find(|item| item["role"] == "user"))
        .and_then(|item| {
            item["content"].as_str().or_else(|| {
                item["content"]
                    .as_array()?
                    .iter()
                    .find_map(|part| part["text"].as_str())
            })
        })
        .unwrap_or("fixture");
    let text = json!({"type":"response.output_text.delta", "delta":format!("fixture answer for: {prompt}")});
    let complete = json!({"type":"response.completed", "response":{"usage":{"input_tokens":7,"output_tokens":5,"total_tokens":12}}});
    let body = script.unwrap_or_else(|| format!("data: {text}\n\ndata: {complete}\n\n"));
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
    Ok(())
}

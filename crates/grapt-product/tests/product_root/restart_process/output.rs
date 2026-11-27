//! Continuously drain a private pipe while retaining only its bounded tail.
use std::collections::VecDeque;
use std::io::{Error, ErrorKind, Read};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

const TAIL_BYTES: usize = 4096;

pub(super) struct Output {
    tail: Arc<Mutex<Tail>>,
    worker: JoinHandle<()>,
}

impl Output {
    pub(super) fn start(pipe: impl Read + Send + 'static) -> Self {
        let tail = Arc::new(Mutex::new(Tail {
            bytes: VecDeque::with_capacity(TAIL_BYTES),
            total: 0,
            error: None,
        }));
        let captured = Arc::clone(&tail);
        let worker = thread::spawn(move || drain(pipe, &captured));
        Self { tail, worker }
    }

    pub(super) fn collect(output: Option<Self>, reaped: bool) -> String {
        let Some(output) = output else {
            return "tail unavailable: capture already collected".to_owned();
        };
        // If process cleanup failed, retain the current snapshot without
        // waiting for an EOF that the live process may never send.
        let worker = if reaped {
            if output.worker.join().is_ok() {
                "joined"
            } else {
                "panicked"
            }
        } else {
            "unjoined: process cleanup failed"
        };
        let (tail, poisoned) = match output.tail.lock() {
            Ok(tail) => (tail, false),
            Err(error) => (error.into_inner(), true),
        };
        let bytes: Vec<u8> = tail.bytes.iter().copied().collect();
        format!("retained tail (total={}B, retained={}B, cap={TAIL_BYTES}B, truncated={}, read_error={:?}, worker={worker}, poisoned={poisoned})={:?}",
            tail.total, bytes.len(), tail.total > bytes.len() as u64, tail.error, String::from_utf8_lossy(&bytes))
    }
}

struct Tail {
    bytes: VecDeque<u8>,
    total: u64,
    error: Option<Error>,
}

fn drain(mut pipe: impl Read, captured: &Mutex<Tail>) {
    let mut buffer = [0; 1024];
    loop {
        let read = pipe.read(&mut buffer);
        if matches!(&read, Err(error) if error.kind() == ErrorKind::Interrupted) {
            continue;
        }
        let mut tail = captured.lock().unwrap_or_else(|error| error.into_inner());
        match read {
            Ok(0) => return,
            Ok(count) => {
                tail.total += count as u64;
                for &byte in &buffer[..count] {
                    if tail.bytes.len() == TAIL_BYTES {
                        tail.bytes.pop_front();
                    }
                    tail.bytes.push_back(byte);
                }
            }
            Err(error) => {
                tail.error = Some(error);
                return;
            }
        }
    }
}

//! Optional request-line observations for a native reconnect, without credentials.
use super::*;

#[derive(Default)]
pub(super) struct RequestLine {
    bytes: Vec<u8>,
    done: bool,
}

impl RequestLine {
    pub(super) fn observe(&mut self, chunk: &[u8], state: &Mutex<Relayed>) {
        if self.done {
            return;
        }
        let mut state = locked(state);
        let Some(trace) = state.trace.as_mut() else {
            self.done = true;
            return;
        };
        let count = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(chunk.len(), |at| at + 1);
        assert!(self.bytes.len() + count <= 8192, "relay request-line bound");
        self.bytes.extend_from_slice(&chunk[..count]);
        if self.bytes.last() == Some(&b'\n') {
            assert!(trace.len() < 256, "relay request-trace bound");
            let line = std::str::from_utf8(&self.bytes)
                .unwrap_or_else(|error| panic!("relay request line: {error}"));
            trace.push(line.trim_end().to_owned());
            state.trace_changed.notify_all();
            self.done = true;
            self.bytes.clear();
        }
    }
}

//! Bounded parent-side timing for one local MCP launch generation.
use std::time::Duration;

#[derive(Debug)]
pub struct StartupTrace {
    server: String,
    incarnation: u64,
    started: tokio::time::Instant,
    initialize_written: bool,
    stdout_seen: bool,
}

impl StartupTrace {
    pub fn new(server: &str, incarnation: u64) -> Self {
        Self {
            server: server.chars().take(64).collect(),
            incarnation,
            started: tokio::time::Instant::now(),
            initialize_written: false,
            stdout_seen: false,
        }
    }

    fn line(&self, stage: &'static str, elapsed: Duration) -> String {
        format!(
            "MCP_STARTUP server={:?} launch={} stage={} elapsed_ms={}",
            self.server,
            self.incarnation,
            stage,
            elapsed.as_millis()
        )
    }

    pub fn mark(&self, stage: &'static str) {
        // Only the public server label and existing launch generation are logged.
        // Command, environment, wire bytes and tool arguments stay private.
        grapt_diagnostics::report(self.line(stage, self.started.elapsed()));
    }

    pub(crate) fn wrote_initialize(&mut self) {
        if !self.initialize_written {
            self.initialize_written = true;
            self.mark("initialize-write");
        }
    }

    pub(crate) fn read_stdout(&mut self) {
        if !self.stdout_seen {
            self.stdout_seen = true;
            self.mark("first-stdout-byte");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_hostile_server_label_cannot_forge_or_flood_timing_records() {
        let trace = StartupTrace::new(&format!("server\n\r\t{}", "x".repeat(10_000)), 7);
        let line = trace.line("spawn", Duration::from_millis(3));
        assert!(!line.chars().any(char::is_control));
        assert!(line.len() < 1_024);
        assert!(line.contains("launch=7 stage=spawn elapsed_ms=3"));
    }
}

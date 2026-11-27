//! Model-visible process observations; exit, pipe drain and cleanup are independent.
use super::output::Page;
use grapt_shell::{CleanupState, PipeState, ShellTerminal};
use grapt_types::OperationId;

pub(super) struct ProcessReport {
    pub sandbox_notes: Vec<String>,
    pub enforcement: grapt_types::RealizedEnforcement,
    pub origin: Option<super::recovery::Origin>,
    pub operation: OperationId,
    pub exit: Option<ShellTerminal>,
    pub pipe: PipeState,
    pub cleanup: CleanupState,
    pub cleanup_error: Option<String>,
    pub interrupt: Option<String>,
    pub total_bytes: u64,
    pub page: Page,
}

impl ProcessReport {
    pub fn encode(self) -> Result<Vec<u8>, String> {
        let settled = self.exit.is_some()
            && self.pipe.is_settled()
            && (self.cleanup == CleanupState::Complete || self.cleanup_error.is_some());
        let mut output = match self.page.omitted_at {
            Some(at) => format!(
                "{}\n[shell: omitted {} output bytes]\n{}",
                String::from_utf8_lossy(&self.page.bytes[..at]),
                self.page.omitted,
                String::from_utf8_lossy(&self.page.bytes[at..])
            ),
            None => String::from_utf8_lossy(&self.page.bytes).into_owned(),
        };
        for note in &self.sandbox_notes {
            output.push_str(&format!("\n[{note}]"));
        }
        let mut result = serde_json::json!({
            "session_id":self.operation.as_str(),
            "enforcement":super::super::sandbox::value(&self.enforcement),
            "ran":true,
            "output":output,
            "output_cursor":self.page.cursor,
            "omitted_bytes":self.page.omitted,
            "has_more_output":self.page.cursor < self.total_bytes,
            "done":settled,
            "pipe":self.pipe.as_str(),
            "cleanup":if self.cleanup == CleanupState::Complete { "complete" } else { "incomplete" },
        });
        if !self.sandbox_notes.is_empty() {
            result["sandbox_notes"] = serde_json::json!(self.sandbox_notes);
        }
        if let Some(origin) = self.origin {
            result["recovery"] = serde_json::to_value(origin).map_err(|error| error.to_string())?;
        }
        #[cfg(unix)]
        {
            result["containment"] = serde_json::json!("process_group");
            result["notice"] = serde_json::json!(
                "Processes that leave this command's process group (setsid, daemons) may outlive it."
            );
        }
        #[cfg(windows)]
        {
            result["containment"] = serde_json::json!("job");
            result["notice"] = serde_json::json!(
                "The command starts in a cleanup job; descendants that explicitly break away may outlive it."
            );
        }
        match self.exit {
            None => result["status"] = serde_json::json!("running"),
            Some(ShellTerminal::Exited(code)) => {
                result["status"] = serde_json::json!("exited");
                result["exit_code"] = serde_json::json!(code);
                if code != 0
                    && matches!(
                        self.enforcement,
                        grapt_types::RealizedEnforcement::PlatformSandbox { .. }
                    )
                {
                    result["retry_guidance"] = serde_json::json!(
                        "If containment denied a write, request_sandbox_access can ask to retry the command with an exact existing path for this run, this session, or Always for this project. Earlier effects may repeat. Stderr does not grant access; protected paths remain ungrantable."
                    );
                }
            }
            Some(ShellTerminal::TimedOut) => result["status"] = serde_json::json!("timed_out"),
            Some(ShellTerminal::Cancelled) => result["status"] = serde_json::json!("cancelled"),
            Some(ShellTerminal::Failed(reason)) => {
                result["status"] = serde_json::json!("failed");
                result["error"] = serde_json::json!(reason);
            }
        }
        if let Some(reason) = self.cleanup_error {
            result["cleanup_error"] = serde_json::json!(reason);
        }
        if let Some(interrupt) = self.interrupt {
            result["interrupt"] = serde_json::json!(interrupt);
        }
        serde_json::to_vec(&result).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_markers_render_lossy_sides_and_exact_counts() -> Result<(), String> {
        let page = Page {
            bytes: vec![0xf0, 0x9f, b'b', b'b'],
            omitted: 10,
            omitted_at: Some(2),
            cursor: 100,
        };
        let report = ProcessReport {
            sandbox_notes: Vec::new(),
            enforcement: grapt_types::RealizedEnforcement::HarnessOnly,
            origin: None,
            operation: OperationId::new("op_utf8marker01").ok_or("test identity refused")?,
            exit: None,
            pipe: PipeState::Drained,
            cleanup: CleanupState::Complete,
            cleanup_error: None,
            interrupt: None,
            total_bytes: 1000,
            page,
        };
        let encoded = serde_json::from_slice::<serde_json::Value>(&report.encode()?)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            encoded["output"],
            "\u{FFFD}\n[shell: omitted 10 output bytes]\nbb"
        );
        assert_eq!(encoded["output_cursor"], 100);
        assert_eq!(encoded["omitted_bytes"], 10);
        assert_eq!(encoded["has_more_output"], true);
        assert_eq!(encoded["status"], "running");
        let page = Page {
            bytes: vec![0xf0, 0x9f, 0x98, 0x80],
            omitted: 3,
            omitted_at: Some(2),
            cursor: 4,
        };
        let report = ProcessReport {
            sandbox_notes: Vec::new(),
            enforcement: grapt_types::RealizedEnforcement::HarnessOnly,
            origin: None,
            operation: OperationId::new("op_utf8marker02").ok_or("test identity refused")?,
            exit: None,
            pipe: PipeState::Open,
            cleanup: CleanupState::Pending,
            cleanup_error: None,
            interrupt: None,
            total_bytes: 4,
            page,
        };
        let encoded = serde_json::from_slice::<serde_json::Value>(&report.encode()?)
            .map_err(|error| error.to_string())?;
        assert_eq!(
            encoded["output"],
            "\u{FFFD}\n[shell: omitted 3 output bytes]\n\u{FFFD}\u{FFFD}"
        );
        assert_eq!(encoded["omitted_bytes"], 3);
        assert_eq!(encoded["has_more_output"], false);
        Ok(())
    }
}

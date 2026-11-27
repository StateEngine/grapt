//! Application connection binding for MCP status.
pub(crate) type ServerEntry = grapt_mcp_host::entry::ServerEntry<super::LiveServer>;

impl grapt_mcp_host::entry::LiveStatus for super::LiveServer {
    fn observed_unusable(&self) -> Option<grapt_mcp::session::Unusable> {
        self.observed_unusable()
    }

    fn identity(&self) -> &grapt_mcp::ServerIdentity {
        self.identity()
    }

    fn catalog_len(&self) -> usize {
        self.catalog.len()
    }

    fn stderr_tail(&self) -> Option<String> {
        self.stderr.as_ref().map(|tail| tail.text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_mcp::session::Unusable;

    #[test]
    fn a_notice_reason_never_carries_what_the_server_said() {
        let refused = ServerEntry::unusable(Unusable::Failed(
            "the server refused: token sk-live-123 is invalid (code -32001)".to_owned(),
        ));
        let reason = refused
            .unavailable_reason()
            .unwrap_or_else(|| panic!("a failed server is unavailable"));
        assert!(!reason.contains("sk-live-123"), "{reason}");
        assert_eq!(
            ServerEntry::unusable_in(Unusable::TimedOut, super::super::StartPhase::Discovery)
                .unavailable_reason()
                .as_deref(),
            Some("tool discovery did not complete within its catalog budget")
        );
        assert_eq!(ServerEntry::Disabled.unavailable_reason(), None);
        assert_eq!(
            ServerEntry::unusable(Unusable::Disabled).unavailable_reason(),
            None
        );
    }
}

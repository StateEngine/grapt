//! Fixed recovery guidance for unavailable exact-principal continuity.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuityFailure {
    Binding,
    Capability,
    Contract,
    Principal,
    Incomplete,
    Bounds,
    Transport,
    ServerUnavailable(StateEngineServerFailure),
    TimedOut { seconds: u64 },
    Rebound,
    PacketMissing,
    PacketUnreadable,
    PacketInvalid,
    PacketAcknowledgement,
}

/// Closed diagnostics for server availability; never contains stderr or handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateEngineServerFailure {
    NotConfigured,
    ConfigurationBlocked,
    Disabled,
    Starting,
    HandshakeTimeout,
    DiscoveryTimeout,
    TimedOut,
    Disconnected,
    Failed,
}

impl StateEngineServerFailure {
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotConfigured => "no StateEngine connection is configured",
            Self::ConfigurationBlocked => "the StateEngine declaration is repository-owned and blocked; move it to your user config",
            Self::Disabled => "the configured server is disabled",
            Self::Starting => "startup has not completed",
            Self::HandshakeTimeout => "the startup handshake timed out",
            Self::DiscoveryTimeout => "tool discovery timed out",
            Self::TimedOut => "the server did not answer within its budget",
            Self::Disconnected => "the server closed its channel",
            Self::Failed => "the configured MCP connection failed; /mcps has the reason",
        }
    }
}

impl ContinuityFailure {
    /// Product-authored text only: no identity, capability, cursor or remote
    /// error text can enter the public failure message.
    pub fn message(self) -> String {
        let message = match self {
            Self::Binding => "StateEngine continuity could not read this Session's identity binding. Restore the binding, then send a follow-up prompt.",
            Self::Capability => "StateEngine continuity needs this Session to be rebound after a restart or missing capability. Rebind the Session, then send a follow-up prompt.",
            Self::Contract => "StateEngine continuity is unavailable through the configured MCP connection. Check the StateEngine server and its resume support, then retry.",
            Self::Principal => "StateEngine returned continuity for a different or unverified identity. Check this Session's identity binding and rebind before retrying.",
            Self::Incomplete => "StateEngine continuity is incomplete or stale. Refresh the StateEngine source and send a follow-up prompt; no incomplete context was used.",
            Self::Bounds => "StateEngine continuity exceeded the supported reply or context bound. Reduce the returned context, then send a follow-up prompt.",
            Self::TimedOut { seconds } => return format!("StateEngine continuity timed out after {seconds} s. Check its connection in /mcps, then send a follow-up prompt to retry."),
            Self::ServerUnavailable(reason) => return format!("StateEngine server unavailable: {}. Check /mcps, then send a follow-up prompt to retry.", reason.reason()),
            Self::Transport => "StateEngine continuity could not reach the configured server. Check its connection in /mcps, then send a follow-up prompt to retry.",
            Self::PacketMissing => "StateEngine packet delivery failed: the launch packet is absent. Repair packet delivery and relaunch before coding.",
            Self::PacketUnreadable => "StateEngine packet delivery failed: the launch packet is unreadable or not a regular file. Repair packet delivery and relaunch before coding.",
            Self::PacketInvalid => "StateEngine packet delivery failed: the pointer or packet bytes do not match the launcher's verified delivery. Repair packet delivery and relaunch before coding.",
            Self::PacketAcknowledgement => "StateEngine did not acknowledge this launch packet. Repair the launch binding and relaunch before coding.",
            Self::Rebound => "This Session's StateEngine binding changed while continuity was loading. Send a follow-up prompt to load context for the current binding.",
        };
        message.to_owned()
    }
}

impl std::fmt::Display for ContinuityFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ContinuityFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuity_recovery_is_bounded_product_copy_with_distinct_actions() {
        for failure in [
            ContinuityFailure::Binding,
            ContinuityFailure::Capability,
            ContinuityFailure::Contract,
            ContinuityFailure::Principal,
            ContinuityFailure::Incomplete,
            ContinuityFailure::Bounds,
            ContinuityFailure::Transport,
            ContinuityFailure::ServerUnavailable(StateEngineServerFailure::Disconnected),
            ContinuityFailure::TimedOut { seconds: 60 },
            ContinuityFailure::Rebound,
            ContinuityFailure::PacketMissing,
            ContinuityFailure::PacketUnreadable,
            ContinuityFailure::PacketInvalid,
            ContinuityFailure::PacketAcknowledgement,
        ] {
            let public = super::super::Failure::Continuity(failure).public();
            assert_eq!(public.name, "ContextLimit");
            assert!(public.message.len() <= super::super::MAX_FAILURE_MESSAGE_BYTES);
            assert!(!public.message.chars().any(char::is_control));
            assert!(public.message.contains("StateEngine"));
        }
        for seconds in [5, 60] {
            let failure = ContinuityFailure::TimedOut { seconds };
            let public = super::super::Failure::Continuity(failure).public();
            assert!(public
                .message
                .contains(&format!("timed out after {seconds} s")));
            assert!(public.message.contains("/mcps"));
        }
        assert!(ContinuityFailure::Capability.message().contains("Rebind"));
        assert!(ContinuityFailure::Transport.message().contains("/mcps"));
        assert!(ContinuityFailure::Principal
            .message()
            .contains("unverified"));
    }
}

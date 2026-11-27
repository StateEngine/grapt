//! Provider-compaction fallback diagnostics from closed failure classes.
use grapt_types::{AttemptTerminalReason, ProviderFailureClass};

pub(super) fn failed(reason: &AttemptTerminalReason) -> &'static str {
    match reason {
        AttemptTerminalReason::ProviderFailure { class } => match class {
            ProviderFailureClass::Transport => "interrupted",
            ProviderFailureClass::NoProgressTimeout => "no-progress-timeout",
            ProviderFailureClass::InvalidProviderOutput => "malformed",
            ProviderFailureClass::RateLimit
            | ProviderFailureClass::ProviderInternal
            | ProviderFailureClass::Authentication
            | ProviderFailureClass::Quota
            | ProviderFailureClass::ContentPolicy
            | ProviderFailureClass::InvalidRequest
            | ProviderFailureClass::UnknownProvider
            | ProviderFailureClass::NoRoute
            | ProviderFailureClass::LocalRefusal => "refused",
        },
        AttemptTerminalReason::Interrupted { .. } => "interrupted",
        _ => "refused",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_fallback_keeps_transport_malformed_and_timeout_distinct() {
        for (class, expected) in [
            (ProviderFailureClass::Transport, "interrupted"),
            (
                ProviderFailureClass::NoProgressTimeout,
                "no-progress-timeout",
            ),
            (ProviderFailureClass::InvalidProviderOutput, "malformed"),
            (ProviderFailureClass::InvalidRequest, "refused"),
        ] {
            assert_eq!(
                failed(&AttemptTerminalReason::ProviderFailure { class }),
                expected
            );
        }
    }
}

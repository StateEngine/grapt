use super::*;
use crate::{
    CursorInvalidReason, CursorInvalidation, CursorRecovery, MailTransportClientInstanceId,
    SafeReplayOrigin, SourcePrincipalId,
};

#[test]
fn operation_diagnostics_retain_the_refusal_without_cursor_or_takeover_handles(
) -> Result<(), Box<dyn std::error::Error>> {
    let failures = [
        SubscriptionClosed::DisplacedByTakeover {
            by_principal: SourcePrincipalId::try_new("private-principal")?,
            by_client: MailTransportClientInstanceId::try_new("private-client")?,
        }
        .into(),
        TransportFailure::CursorInvalid(CursorInvalidation::new(
            CursorInvalidReason::Expired,
            CursorRecovery::ReplayFrom(SafeReplayOrigin::try_new("private-replay-origin")?),
        ))
        .into(),
    ];
    for failure in failures {
        let failure: OperationFailure = failure;
        let detail = failure.to_string();
        assert!(!detail.contains("private-"));
        assert!(detail.len() <= crate::limits::GRAPT_MAX_DIAGNOSTIC_BYTES);
        assert!(
            detail.contains("DisplacedByTakeover") || detail.contains("CursorInvalid(expired)")
        );
    }
    let failure: OperationFailure = DeliveryAckFailure::MessageNeverEnumerated.into();
    assert!(failure
        .to_string()
        .contains("Delivery(MessageNeverEnumerated)"));
    assert!(failure.to_string().contains("effect None"));
    Ok(())
}

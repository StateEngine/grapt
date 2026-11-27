use std::collections::BTreeSet;

use grapt_schema::{tri_state_ledger as ledger, OptionalNullable, TriStateEvidence};

macro_rules! frozen_rows {
    ($( $marker:ident => $row:literal ),+ $(,)?) => {
        const EXPECTED: &[&str] = &[$($row),+];

        fn assert_sealed_carriers() {
            fn carrier<E: TriStateEvidence>(expected: &str) {
                assert_eq!(E::LEDGER_ROW, expected);
                let _ = OptionalNullable::<bool, E>::null();
            }
            $(carrier::<ledger::$marker>($row);)+
        }
    };
}

frozen_rows! {
    U3aConflictErrorResource => "schema:ConflictError.resource",
    U3aInvalidRequestErrorKind => "schema:InvalidRequestError.kind",
    U3aInvalidRequestErrorField => "schema:InvalidRequestError.field",
    U3aInvalidRequestError1Kind => "schema:InvalidRequestError1.kind",
    U3aInvalidRequestError1Field => "schema:InvalidRequestError1.field",
    U3aServiceUnavailableErrorService => "schema:ServiceUnavailableError.service",
    U3aSessionActiveExecutionId => "schema:SessionActive.executionID",
    U3aSessionMessagesCursorPrevious => "schema:SessionMessagesResponse.cursor.previous",
    U3aSessionMessagesCursorNext => "schema:SessionMessagesResponse.cursor.next",
    U3aSessionsCursorPrevious => "schema:SessionsResponse.cursor.previous",
    U3aSessionsCursorNext => "schema:SessionsResponse.cursor.next",
    U3aToolFileContentName => "schema:Tool.FileContent.name",
    U3aUnknownErrorRef => "schema:UnknownError.ref",
    U3aProjectCurrentLocation => "operation:GET /api/project/current.query.location",
    U3aProjectCurrentLocationDirectory => "operation:GET /api/project/current.query.location.directory",
    U3aProjectCurrentLocationWorkspace => "operation:GET /api/project/current.query.location.workspace",
    U3aProviderLocation => "operation:GET /api/provider.query.location",
    U3aProviderLocationDirectory => "operation:GET /api/provider.query.location.directory",
    U3aProviderLocationWorkspace => "operation:GET /api/provider.query.location.workspace",
    U3aModelLocation => "operation:GET /api/model.query.location",
    U3aModelLocationDirectory => "operation:GET /api/model.query.location.directory",
    U3aModelLocationWorkspace => "operation:GET /api/model.query.location.workspace",
    U3aModelDefaultLocation => "operation:GET /api/model/default.query.location",
    U3aModelDefaultLocationDirectory => "operation:GET /api/model/default.query.location.directory",
    U3aModelDefaultLocationWorkspace => "operation:GET /api/model/default.query.location.workspace",
    U3aAgentLocation => "operation:GET /api/agent.query.location",
    U3aAgentLocationDirectory => "operation:GET /api/agent.query.location.directory",
    U3aAgentLocationWorkspace => "operation:GET /api/agent.query.location.workspace",
    U3aSessionListWorkspace => "operation:GET /api/session.query.workspace",
    U3aSessionListLimit => "operation:GET /api/session.query.limit",
    U3aSessionListOrder => "operation:GET /api/session.query.order",
    U3aSessionListSearch => "operation:GET /api/session.query.search",
    U3aSessionListParentId => "operation:GET /api/session.query.parentID",
    U3aSessionListDirectory => "operation:GET /api/session.query.directory",
    U3aSessionListProject => "operation:GET /api/session.query.project",
    U3aSessionListSubpath => "operation:GET /api/session.query.subpath",
    U3aSessionListCursor => "operation:GET /api/session.query.cursor",
    U3aMessageListLimit => "operation:GET /api/session/{sessionID}/message.query.limit",
    U3aMessageListOrder => "operation:GET /api/session/{sessionID}/message.query.order",
    U3aMessageListCursor => "operation:GET /api/session/{sessionID}/message.query.cursor",
    U3aSessionCreateId => "operation:POST /api/session.body.id",
    U3aSessionCreateAgent => "operation:POST /api/session.body.agent",
    U3aSessionCreateModel => "operation:POST /api/session.body.model",
    U3aSessionCreateLocation => "operation:POST /api/session.body.location",
    U3aPromptId => "operation:POST /api/session/{sessionID}/prompt.body.id",
    U3aPromptDelivery => "operation:POST /api/session/{sessionID}/prompt.body.delivery",
    U3aPromptResume => "operation:POST /api/session/{sessionID}/prompt.body.resume",
    U3aInterruptExecutionId => "operation:POST /api/session/{sessionID}/interrupt.body.executionID",
}

#[test]
fn exact_48_frozen_contract_rows_are_unique_and_attributable() {
    let actual = ledger::U3A_OPTIONAL_NULLABLE_LEDGER;
    assert_eq!(actual, EXPECTED);
    assert_eq!(actual.len(), 48);
    assert_eq!(actual.iter().copied().collect::<BTreeSet<_>>().len(), 48);
    assert!(actual
        .iter()
        .all(|row| row.starts_with("schema:") || row.starts_with("operation:")));
    assert_sealed_carriers();
}

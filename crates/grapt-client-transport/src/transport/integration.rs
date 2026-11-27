//! Integration addresses use the engine's parsed DTO identities.
use super::*;
use crate::wire::Spelled;
use grapt_api::u3a::{CredentialIdDto, IntegrationAttemptIdDto, IntegrationIdDto};

impl WireRequest {
    pub fn integration(
        operation: Operation,
        integration: &IntegrationIdDto,
        attempt: Option<&IntegrationAttemptIdDto>,
    ) -> Result<Self, TransportError> {
        if !matches!(
            operation,
            Operation::IntegrationKeyConnect
                | Operation::IntegrationOAuthConnect
                | Operation::IntegrationOAuthStatus
                | Operation::IntegrationOAuthCancel
                | Operation::IntegrationOAuthComplete
        ) {
            return Err(TransportError::Contract(
                crate::contract::ContractError::RequestRequired {
                    operation: operation.operation_id(),
                },
            ));
        }
        let mut path = operation
            .template()
            .replace("{integrationID}", &segment(integration.spelled()));
        if path.contains("{attemptID}") {
            let attempt = attempt.ok_or(TransportError::Contract(
                crate::contract::ContractError::RequestRequired {
                    operation: operation.operation_id(),
                },
            ))?;
            path = path.replace("{attemptID}", &segment(attempt.spelled()));
        }
        Ok(Self {
            operation,
            path,
            query: Vec::new(),
            body: None,
        })
    }

    pub fn credential(
        operation: Operation,
        credential: &CredentialIdDto,
    ) -> Result<Self, TransportError> {
        if !matches!(
            operation,
            Operation::CredentialRename | Operation::CredentialRemove
        ) {
            return Err(TransportError::Contract(
                crate::contract::ContractError::RequestRequired {
                    operation: operation.operation_id(),
                },
            ));
        }
        Ok(Self {
            operation,
            path: operation
                .template()
                .replace("{credentialID}", &segment(credential.spelled())),
            query: Vec::new(),
            body: None,
        })
    }
}
fn segment(value: &str) -> String {
    value.bytes().map(|b| format!("%{b:02X}")).collect()
}

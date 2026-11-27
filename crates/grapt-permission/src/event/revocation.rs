//! Strict, bounded coordinates for withdrawing existing project approvals.
use super::*;

pub const MAX_RULE_ORIGINS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRuleRevocation {
    pub session: SessionId,
    pub project: ProjectId,
    pub action: String,
    pub resource: String,
    pub origins: Vec<PermissionId>,
    pub owner_subject: String,
    pub owner_client: AuthenticatedClientId,
}

impl PermissionRuleRevocation {
    pub(crate) fn decode(data: &Value) -> Result<Self, PermissionEventError> {
        let project = nonempty_str(data, "projectID")?;
        let project =
            ProjectId::new(project.clone()).ok_or(PermissionEventError::OutsideVocabulary {
                field: "projectID",
                value: project,
            })?;
        let client = nonempty_str(data, "ownerClient")?;
        let owner_client = AuthenticatedClientId::new(client.clone()).ok_or(
            PermissionEventError::OutsideVocabulary {
                field: "ownerClient",
                value: client,
            },
        )?;
        let action = bounded(data, "action", 1024, false)?;
        let resource = bounded(data, "resource", 8192, true)?;
        let owner_subject = bounded(data, "ownerSubject", 512, false)?;
        let origins = string_array(data, "originRequests", MAX_RULE_ORIGINS)?;
        if origins.is_empty() {
            return Err(PermissionEventError::OutsideVocabulary {
                field: "originRequests",
                value: "empty".into(),
            });
        }
        let mut unique = std::collections::BTreeSet::new();
        for raw in origins {
            if raw.len() > 512 {
                return Err(PermissionEventError::ValueBound {
                    field: "originRequests",
                    max: 512,
                });
            }
            let id = raw
                .parse()
                .map_err(|source| PermissionEventError::Identity {
                    field: "originRequests",
                    source,
                })?;
            if !unique.insert(id) {
                return Err(PermissionEventError::OutsideVocabulary {
                    field: "originRequests",
                    value: "duplicate approval".into(),
                });
            }
        }
        Ok(Self {
            session: identity(data, "sessionID")?,
            project,
            action,
            resource,
            origins: unique.into_iter().collect(),
            owner_subject,
            owner_client,
        })
    }
}

fn bounded(
    data: &Value,
    field: &'static str,
    max: usize,
    empty: bool,
) -> Result<String, PermissionEventError> {
    let value = required_str(data, field)?;
    if !empty && value.is_empty() {
        return Err(PermissionEventError::OutsideVocabulary {
            field,
            value: String::new(),
        });
    }
    if value.len() > max {
        return Err(PermissionEventError::ValueBound { field, max });
    }
    Ok(value.to_owned())
}

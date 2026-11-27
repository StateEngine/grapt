//! Saved coordinates in the contract double must survive a fresh client.
use super::*;

impl ContractDouble {
    pub(super) fn session_snapshot(
        &self,
        id: &SessionWireId,
    ) -> Option<Result<Vec<u8>, crate::wire::WireError>> {
        self.sessions.get(id).map(|entry| {
            let directory = entry
                .project
                .as_ref()
                .map_or("/work", |row| row.worktree.as_str());
            let info =
                crate::wire::session_info(id, &entry.title, directory, entry.parent.as_ref())?;
            let mut value = serde_json::to_value(info).map_err(snapshot_error)?;
            if let Some(project) = &entry.project {
                value["projectID"] = serde_json::to_value(&project.id).map_err(snapshot_error)?;
            }
            if let Some(agent) = &entry.agent {
                value["agent"] = serde_json::to_value(agent).map_err(snapshot_error)?;
            }
            let info = serde_json::from_value(value).map_err(snapshot_error)?;
            let model = entry.model.as_ref().map(|(id, provider)| {
                grapt_api::u3a::ModelRefDto::new(
                    grapt_schema::Required::new(id.clone()),
                    grapt_schema::Required::new(provider.clone()),
                    entry.variant.clone().map_or_else(
                        grapt_schema::Optional::absent,
                        grapt_schema::Optional::present,
                    ),
                )
            });
            crate::wire::encode_session_with_model(info, model)
        })
    }
}

fn snapshot_error(error: serde_json::Error) -> crate::wire::WireError {
    crate::wire::WireError::Invalid {
        what: "double session snapshot",
        value: error.to_string(),
    }
}

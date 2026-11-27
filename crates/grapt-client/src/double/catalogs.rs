//! Read-only project catalogs and workspace snapshots in the contract double.
//! All location resolution and producer state remain on ContractDouble.
use super::*;

impl ContractDouble {
    pub(super) fn catalog_route(
        &self,
        request: &WireRequest,
    ) -> Result<WireResponse, TransportError> {
        match request.operation() {
            Operation::WorkspaceDiff => {
                let Some(project) = self.located(request) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                let body = crate::wire::encode_workspace_diff(
                    project,
                    "UNSTAGED\ndiff --git a/fixture.txt b/fixture.txt\n-before\n+after\n",
                )
                .map_err(|error| TransportError::Io {
                    reason: error.to_string(),
                })?;
                Ok(WireResponse { status: 200, body })
            }
            Operation::ProjectList => match crate::wire::encode_projects(&self.projects) {
                Ok(body) => Ok(WireResponse { status: 200, body }),
                Err(_) => Ok(WireResponse {
                    status: 500,
                    body: Vec::new(),
                }),
            },
            Operation::ProjectCurrent => match self.located(request) {
                Some(project) => match crate::wire::encode_project_current(project) {
                    Ok(body) => Ok(WireResponse { status: 200, body }),
                    Err(_) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                },
                // A directory that resolves to no project is a 400 from the
                // frozen route, not an invented empty project.
                None => Ok(WireResponse {
                    status: 400,
                    body: Vec::new(),
                }),
            },
            Operation::AgentList => {
                let Some(project) = self.located(request) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                match crate::wire::encode_agents(project, &self.agents) {
                    Ok(body) => Ok(WireResponse { status: 200, body }),
                    Err(_) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                }
            }
            Operation::ModelList => {
                let Some(project) = self.located(request) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                match crate::wire::encode_models(project, &self.models) {
                    Ok(body) => Ok(WireResponse { status: 200, body }),
                    Err(error) => {
                        // The reason, not just the status: a 500 with no cause
                        // sent a witness looking in the wrong place.
                        eprintln!("double: model list encode refused: {error}");
                        Ok(WireResponse {
                            status: 500,
                            body: Vec::new(),
                        })
                    }
                }
            }
            Operation::ModelDefault => {
                let Some(project) = self.located(request) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                // The default is the first ENABLED model, so the double never
                // hands back a default the picker itself would refuse.
                let default = self.models.iter().find(|model| model.enabled);
                match crate::wire::encode_model_default(project, default) {
                    Ok(body) => Ok(WireResponse { status: 200, body }),
                    Err(_) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                }
            }
            _ => Err(TransportError::NotConnected),
        }
    }
}

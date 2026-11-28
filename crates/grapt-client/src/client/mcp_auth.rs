//! Bounded MCP operator actions share the DTO ingress, independent of live events.
use super::*;
use grapt_api::u3a::{
    BoundedString, McpApprovalPreviewDto, McpAuthRequestDto, McpAuthResponseDto, McpAuthState,
    McpConfigurationRequestDto, WireId,
};

#[derive(Debug)]
enum Request {
    Auth(McpAuthRequestDto),
    Configuration(McpConfigurationRequestDto),
    Reconnect,
    ApprovalPreview,
    Refresh { message: String },
}

impl Request {
    fn operation(&self) -> Operation {
        match self {
            Self::Auth(_) => Operation::McpAuth,
            Self::Configuration(_) => Operation::McpConfiguration,
            Self::Reconnect => Operation::McpReconnect,
            Self::ApprovalPreview => Operation::McpApproval,
            Self::Refresh { .. } => Operation::McpInspect,
        }
    }

    fn body(&self) -> Result<Option<Vec<u8>>, serde_json::Error> {
        match self {
            Self::Auth(request) => serde_json::to_vec(request).map(Some),
            Self::Configuration(request) => serde_json::to_vec(request).map(Some),
            Self::Reconnect | Self::ApprovalPreview | Self::Refresh { .. } => Ok(None),
        }
    }
}

#[derive(Debug)]
pub(super) struct PendingLogin {
    directory: String,
    session: Option<SessionWireId>,
    server: String,
    id: WireId<grapt_types::McpLoginId>,
}
#[derive(Debug)]
pub(super) struct PendingApproval {
    directory: String,
    session: Option<SessionWireId>,
    server: String,
    fingerprint: BoundedString<64>,
}
#[derive(Debug)]
pub(super) struct InFlight {
    directory: String,
    session: Option<SessionWireId>,
    server: String,
    request: Request,
    startup_location: Option<crate::wire::FormLocation>,
    existing_forms: std::collections::BTreeSet<crate::wire::FormWireId>,
}
struct Server<'a>(&'a str);
impl crate::wire::Spelled for Server<'_> {
    fn spelled(&self) -> &str {
        self.0
    }
}

impl<C: Connection> Client<C> {
    pub fn mcp_auth_pending(&self) -> bool {
        self.mcp_auth.is_some()
    }

    pub fn begin_mcp_sign_in(&mut self, server: &str) -> Result<Option<Vec<String>>, ClientError> {
        let directory = &self
            .project
            .as_ref()
            .ok_or(ClientError::NoProject)?
            .worktree;
        let request = match self.mcp_login.as_ref().filter(|pending| {
            pending.server == server
                && &pending.directory == directory
                && pending.session == self.selected
        }) {
            Some(pending) => McpAuthRequestDto::Status {
                login_id: pending.id.clone(),
            },
            None => McpAuthRequestDto::Login {
                client_id: None,
                scope: None,
            },
        };
        self.begin_mcp_auth(server, Request::Auth(request))
    }

    pub fn begin_mcp_sign_out(&mut self, server: &str) -> Result<Option<Vec<String>>, ClientError> {
        self.begin_mcp_auth(server, Request::Auth(McpAuthRequestDto::Logout {}))
    }

    pub fn begin_mcp_enabled(
        &mut self,
        server: &str,
        enabled: bool,
    ) -> Result<Option<Vec<String>>, ClientError> {
        self.begin_mcp_auth(
            server,
            Request::Configuration(McpConfigurationRequestDto::SetEnabled { enabled }),
        )
    }

    pub fn begin_mcp_reconnect(
        &mut self,
        server: &str,
    ) -> Result<Option<Vec<String>>, ClientError> {
        self.begin_mcp_auth(server, Request::Reconnect)
    }

    pub fn begin_mcp_approve(&mut self, server: &str) -> Result<Option<Vec<String>>, ClientError> {
        if self.operator_request_pending() || self.mcp_catalog_pending() {
            return Err(ClientError::Mcp(
                "An operator request is already in progress; draft kept.".into(),
            ));
        }
        let directory = &self
            .project
            .as_ref()
            .ok_or(ClientError::NoProject)?
            .worktree;
        let confirmed = self.mcp_approval.as_ref().is_some_and(|pending| {
            pending.server == server
                && &pending.directory == directory
                && pending.session == self.selected
        });
        let request = if confirmed {
            let pending = self
                .mcp_approval
                .take()
                .ok_or_else(|| Self::mcp_body_error("approval preview missing"))?;
            Request::Configuration(McpConfigurationRequestDto::ApproveProject {
                fingerprint: pending.fingerprint,
            })
        } else {
            self.mcp_approval = None;
            Request::ApprovalPreview
        };
        self.begin_mcp_auth(server, request)
    }

    pub fn begin_mcp_revoke(&mut self, server: &str) -> Result<Option<Vec<String>>, ClientError> {
        self.mcp_approval = None;
        self.begin_mcp_auth(
            server,
            Request::Configuration(McpConfigurationRequestDto::RevokeProjectApproval {}),
        )
    }

    fn begin_mcp_auth(
        &mut self,
        server: &str,
        request: Request,
    ) -> Result<Option<Vec<String>>, ClientError> {
        // Host-started I/O never coexists with a pending resume.
        self.abandon_resume();
        if self.operator_request_pending() || self.mcp_catalog_pending() {
            return Err(ClientError::Mcp(
                "An operator request is already in progress; draft kept.".into(),
            ));
        }
        let directory = self
            .project
            .as_ref()
            .ok_or(ClientError::NoProject)?
            .worktree
            .clone();
        let body = request
            .body()
            .map_err(|_| Self::mcp_body_error("request encoding failed"))?;
        let coordinate = Server(server);
        let coordinate: Option<&dyn crate::wire::Spelled> =
            if matches!(request, Request::Refresh { .. }) {
                None
            } else {
                Some(&coordinate)
            };
        let mut wire = WireRequest::new_typed(request.operation(), None, coordinate)?;
        if let Some(body) = body {
            wire = wire.with_body(body);
        }
        if let Some(session) = &self.selected {
            wire = wire.with_query("session", crate::wire::Spelled::spelled(session));
        } else {
            wire = wire.with_query("location[directory]", &directory);
        }
        self.mcp_auth = Some(InFlight {
            startup_location: matches!(request, Request::Refresh { .. })
                .then(|| self.form_location())
                .flatten(),
            existing_forms: self
                .location_forms()
                .iter()
                .map(|row| row.id.clone())
                .collect(),
            directory,
            session: self.selected.clone(),
            server: server.into(),
            request,
        });
        let result = self.begin_operator(&wire);
        self.mcp_auth_result(result)
    }

    pub fn poll_mcp_auth(&mut self) -> Result<Option<Vec<String>>, ClientError> {
        let Some(flight) = &self.mcp_auth else {
            return Ok(None);
        };
        // Configuration/reconnect starts a fresh inspection on the same lane.
        // Like /mcp, only new startup forms at its captured location may spend
        // the HTTP acquisition's existing, finite human allowance.
        let waiting = flight.startup_location.as_ref().is_some_and(|location| {
            self.form_location().as_ref() == Some(location)
                && self.selected == flight.session
                && self
                    .location_forms
                    .startup_wait(location, &flight.existing_forms)
        });
        self.connection.operator_human_wait(waiting);
        let result = self.poll_operator();
        self.mcp_auth_result(result)
    }

    fn mcp_body_error(reason: &str) -> ClientError {
        ClientError::Body {
            operation: Operation::McpAuth.operation_id(),
            reason: reason.into(),
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn mcp_auth_result(
        &mut self,
        result: Result<Option<WireResponse>, ClientError>,
    ) -> Result<Option<Vec<String>>, ClientError> {
        if matches!(result, Ok(None)) {
            return Ok(None);
        }
        let flight = self
            .mcp_auth
            .take()
            .ok_or_else(|| Self::mcp_body_error("no active request"))?;
        if self.project.as_ref().map(|project| &project.worktree) != Some(&flight.directory)
            || self.selected != flight.session
        {
            return Err(ClientError::Mcp(
                "Session or project changed; MCP result discarded.".into(),
            ));
        }
        let checking = matches!(
            flight.request,
            Request::Auth(McpAuthRequestDto::Status { .. })
        );
        let result = result.and_then(|response| {
            let response = response.ok_or_else(|| Self::mcp_body_error("missing response"))?;
            self.accept_response(flight.request.operation(), response)
        });
        let bytes = match result {
            Err(ClientError::Refused {
                status: 400,
                ref message,
                ..
            }) if checking && message.ends_with("unknown sign-in attempt") => {
                self.mcp_login = None;
                return Err(ClientError::Mcp(format!(
                    "Sign-in attempt expired or the service restarted. Run /mcp login {} to start again.",
                    flight.server
                )));
            }
            Err(error) => {
                if let Request::Refresh { message } = flight.request {
                    self.mcp_refusal = Some(error.to_string());
                    return Ok(Some(vec![
                        message,
                        format!("MCP catalog refresh failed: {error}"),
                    ]));
                }
                return Err(error);
            }
            Ok(bytes) => bytes,
        };
        let auth = match flight.request {
            Request::ApprovalPreview => {
                let preview: McpApprovalPreviewDto = serde_json::from_slice(&bytes)
                    .map_err(|_| Self::mcp_body_error("invalid project approval preview"))?;
                let mut lines = vec![
                    format!("PROJECT MCP: {}", flight.server),
                    format!("Directory: {}", preview.directory.as_str().escape_debug()),
                    format!("Source: {}", preview.source.as_str().escape_debug()),
                ];
                lines.extend(
                    preview
                        .description
                        .expose()
                        .as_str()
                        .lines()
                        .map(|line| line.escape_debug().to_string()),
                );
                if preview.approved {
                    lines.push(
                        "Already approved. Use /mcp enable or disable to change its state.".into(),
                    );
                } else {
                    lines.push("Approval permits this project declaration to launch a process or connect to its server.".into());
                    lines.push(format!(
                        "To approve this exact declaration, run /mcp approve {} again.",
                        flight.server
                    ));
                    self.mcp_approval = Some(PendingApproval {
                        directory: flight.directory,
                        session: flight.session,
                        server: flight.server,
                        fingerprint: preview.fingerprint,
                    });
                }
                return Ok(Some(lines));
            }

            Request::Auth(request) => request,
            Request::Configuration(request) => {
                let verb = match request {
                    McpConfigurationRequestDto::SetEnabled { enabled: true } => "Enabled",
                    McpConfigurationRequestDto::SetEnabled { enabled: false } => "Disabled",
                    McpConfigurationRequestDto::ApproveProject { .. } => "Approved",
                    McpConfigurationRequestDto::RevokeProjectApproval {} => "Revoked approval for",
                    _ => "Updated",
                };
                return self.begin_mcp_auth(
                    &flight.server,
                    Request::Refresh {
                        message: format!("{verb} {} in user configuration.", flight.server),
                    },
                );
            }
            Request::Reconnect => {
                return self.begin_mcp_auth(
                    &flight.server,
                    Request::Refresh {
                        message: format!("Retired the previous connection to {}.", flight.server),
                    },
                );
            }
            Request::Refresh { message } => {
                self.refresh_commands();
                let rows = match crate::wire::decode_mcp_inspection(&bytes) {
                    Ok(rows) => rows,
                    Err(error) => {
                        self.mcp_refusal = Some(error.to_string());
                        return Ok(Some(vec![
                            message,
                            format!("MCP catalog refresh failed: {error}"),
                        ]));
                    }
                };
                self.mcp_servers = self.keep_catalog(rows, self.bounds.max_catalog_rows);
                self.mcp_refusal = None;
                let mut lines = vec![message];
                if let Some(row) = self
                    .mcp_servers
                    .iter()
                    .find(|row| row.name == flight.server)
                {
                    lines.push(format!("{}: {}", flight.server, row.status.word()));
                    if let Some(error) = row.status.error() {
                        lines.push(error.escape_debug().to_string());
                    }
                } else {
                    lines.push(format!("{} is no longer declared.", flight.server));
                }
                return Ok(Some(lines));
            }
        };
        let response: McpAuthResponseDto = serde_json::from_slice(&bytes)
            .map_err(|_| Self::mcp_body_error("invalid authentication response"))?;
        let server = flight.server;
        if matches!(auth, McpAuthRequestDto::Logout {}) {
            if response.state != McpAuthState::SignedOut {
                return Err(Self::mcp_body_error("unexpected sign-out state"));
            }
            if self
                .mcp_login
                .as_ref()
                .is_some_and(|pending| pending.server == server)
            {
                self.mcp_login = None;
            }
            return Ok(Some(vec![format!("Signed out of {server}.")]));
        }
        let mut lines = vec![format!("MCP SIGN-IN: {server}")];
        match response.state {
            McpAuthState::AwaitingBrowser => {
                let id = response
                    .login_id
                    .ok_or_else(|| Self::mcp_body_error("missing login identity"))?;
                if let Some(url) = response.authorization_url {
                    lines.push("Open this URL in your browser:".into());
                    lines.push(url.expose().escape_debug().to_string());
                } else if !checking {
                    return Err(Self::mcp_body_error("missing browser URL"));
                }
                self.mcp_login = Some(PendingLogin {
                    directory: flight.directory,
                    session: flight.session,
                    server: server.clone(),
                    id,
                });
                lines.push(format!(
                    "After signing in, run /mcp login {server} again to check."
                ));
            }
            McpAuthState::SignedIn if checking => {
                self.mcp_login = None;
                lines.push(format!("Signed in to {server}."));
            }
            McpAuthState::Failed if checking => {
                self.mcp_login = None;
                lines.push(format!("Sign-in failed. Run /mcp login {server} to retry."));
            }
            _ => return Err(Self::mcp_body_error("unexpected sign-in state")),
        }
        Ok(Some(lines))
    }
}

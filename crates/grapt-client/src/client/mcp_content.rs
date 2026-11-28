//! Content reads share the operator lane; only ordinary user submissions follow.
use super::*;
use crate::wire::{FormLocation, Spelled};
use grapt_api::u3a::{McpContentRequestDto, McpContentResponseDto, McpUserContentDto};

#[derive(Debug)]
pub(super) struct InFlight {
    location: FormLocation,
    session: Option<SessionWireId>,
    kind: ContentKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentKind {
    Resources,
    User,
    Subscription,
}
#[derive(Debug)]
pub enum McpContent {
    Subscription(String),
    User(McpUserContentDto),
    Resources(Vec<String>),
}
struct Server<'a>(&'a str);
impl Spelled for Server<'_> {
    fn spelled(&self) -> &str {
        self.0
    }
}
impl<C: Connection> Client<C> {
    pub fn mcp_content_pending(&self) -> bool {
        self.mcp_content.is_some()
    }

    pub fn begin_mcp_content(
        &mut self,
        server: &str,
        request: McpContentRequestDto,
    ) -> Result<Option<McpContent>, ClientError> {
        // Host-started I/O never coexists with a pending resume.
        self.abandon_resume();
        if self.operator_request_pending() {
            return Err(ClientError::Mcp(
                "An operator request is already in progress.".into(),
            ));
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|session| self.pending.contains_key(session))
        {
            return Err(ClientError::Unconfirmed);
        }
        if matches!(
            request,
            McpContentRequestDto::ReadResource { .. } | McpContentRequestDto::PinResource { .. }
        ) {
            self.create_draft_session().map_err(ClientError::Mcp)?;
            if self.selected.is_none() {
                self.create_session()?;
            }
        }
        let location = self.form_location().ok_or(ClientError::NoProject)?;
        let body =
            serde_json::to_vec(&request).map_err(|error| ClientError::Mcp(error.to_string()))?;
        let mut wire = WireRequest::new_typed(
            Operation::McpContent,
            self.selected.as_ref(),
            Some(&Server(server)),
        )?
        .with_body(body);
        if self.selected.is_none() {
            wire = wire.with_query("location[directory]", &location.directory);
        }
        self.mcp_content = Some(InFlight {
            location,
            session: self.selected.clone(),
            kind: match request {
                McpContentRequestDto::Resources {} => ContentKind::Resources,
                McpContentRequestDto::PinResource { .. } => ContentKind::Subscription,
                _ => ContentKind::User,
            },
        });
        let result = self.begin_operator(&wire);
        self.mcp_content_result(result)
    }

    pub fn poll_mcp_content(&mut self) -> Result<Option<McpContent>, ClientError> {
        if !self.mcp_content_pending() {
            return Ok(None);
        }
        let result = self.poll_operator();
        self.mcp_content_result(result)
    }

    fn mcp_content_result(
        &mut self,
        result: Result<Option<WireResponse>, ClientError>,
    ) -> Result<Option<McpContent>, ClientError> {
        if matches!(result, Ok(None)) {
            return Ok(None);
        }
        let flight = self.mcp_content.take().ok_or(ClientError::NoProject)?;
        if self.selected != flight.session
            || self.form_location().as_ref() != Some(&flight.location)
        {
            return Err(ClientError::Mcp(
                "Session or project changed; MCP content discarded.".into(),
            ));
        }
        let response = result?.ok_or(ClientError::NoProject)?;
        let bytes = self.accept_response(Operation::McpContent, response)?;
        let response: McpContentResponseDto =
            serde_json::from_slice(&bytes).map_err(|error| ClientError::Mcp(error.to_string()))?;
        let (location, content) = match response {
            McpContentResponseDto::Subscription { location, notice }
                if flight.kind == ContentKind::Subscription =>
            {
                (location, McpContent::Subscription(notice.as_str().into()))
            }
            McpContentResponseDto::UserContent { location, content }
                if flight.kind == ContentKind::User =>
            {
                (location, McpContent::User(content))
            }
            McpContentResponseDto::Resources { location, catalog }
                if flight.kind == ContentKind::Resources =>
            {
                let mut lines = vec!["MCP RESOURCES".into()];
                for row in catalog.resources().get().as_slice() {
                    lines.push(format!(
                        "{}  {}",
                        row.name().get().as_str(),
                        row.uri().get().as_str()
                    ));
                    if let Some(description) = row.description().as_present() {
                        lines.push(description.as_str().into());
                    }
                }
                if catalog.resources().get().as_slice().is_empty() {
                    lines.push("No resources advertised.".into());
                }
                if !catalog.templates().get().as_slice().is_empty() {
                    lines.push(
                        "Templates (fill the URI, then use /mcp read <server> <uri>):".into(),
                    );
                    lines.extend(catalog.templates().get().as_slice().iter().map(|row| {
                        format!(
                            "{}  {}",
                            row.name().get().as_str(),
                            row.uri_template().get().as_str()
                        )
                    }));
                }
                (location, McpContent::Resources(lines))
            }
            _ => {
                return Err(ClientError::Mcp(
                    "MCP content response does not match the requested action.".into(),
                ))
            }
        };
        if location.directory().get().as_str() != flight.location.directory
            || location.project().get().id().get() != &flight.location.project
        {
            return Err(ClientError::Mcp(
                "MCP content returned for a different location.".into(),
            ));
        }
        self.mcp_refresh_needed = true;
        let owner = (self.selected.clone(), flight.location);
        if self.mcp_watch_owner.as_ref() != Some(&owner) {
            self.mcp_watches.clear();
        }
        self.mcp_watch_owner = Some(owner);
        Ok(Some(content))
    }
}

impl<C: Connection> Client<C> {
    pub(super) fn mcp_resources_changed(&mut self, scope: &str) -> bool {
        let current = self.command_catalog.accepts_scope(scope)
            && self
                .mcp_watch_owner
                .as_ref()
                .is_some_and(|(session, location)| {
                    *session == self.selected && Some(location) == self.form_location().as_ref()
                });
        self.mcp_refresh_needed |= current;
        current
    }
    pub fn mcp_resource_lines(&self) -> Vec<String> {
        if !self
            .mcp_watch_owner
            .as_ref()
            .is_some_and(|(session, location)| {
                *session == self.selected && Some(location) == self.form_location().as_ref()
            })
        {
            return Vec::new();
        }
        let mut lines = Vec::new();
        for row in &self.mcp_watches {
            let server = row.server.as_str();
            let uri = row.uri.as_str();
            lines.push(format!(
                "{}  {}  {}",
                server,
                uri,
                if row.updated { "updated" } else { "pinned" }
            ));
            if row.updated {
                lines.push(format!(
                    "  Re-read: /mcp read {} {}",
                    quote(server),
                    quote(uri)
                ));
            }
            lines.push(format!(
                "  Unpin: /mcp unpin {} {}",
                quote(server),
                quote(uri)
            ));
        }
        lines
    }
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

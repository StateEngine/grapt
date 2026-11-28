//! Native operator content reads use the same Session selection as MCP controls.
use super::{connections, decode, invalid, session_config, ServerApplication};
use grapt_api::u3a::{
    ApplicationFuture, InvalidRequestErrorDto, McpContentRequestDto, McpContentResponseDto,
    McpResourcesResponseDto, ProjectCurrentLocationQueryDto,
};
use grapt_mcp::content::CatalogKind;
use serde_json::{json, Value};

use grapt_mcp_host::user_content;

pub(crate) fn respond(
    application: ServerApplication,
    server: String,
    query: ProjectCurrentLocationQueryDto,
    request: McpContentRequestDto,
) -> ApplicationFuture<McpContentResponseDto, InvalidRequestErrorDto> {
    Box::pin(async move {
        let selection = session_config::catalog::select(&application, &query)
            .await
            .map_err(invalid)?;
        let snapshot =
            connections::McpSnapshot::read(&selection.sources, &selection.location.directory)
                .map_err(invalid)?;
        snapshot
            .prepare_roots(
                &application.mcp,
                &selection.registry_key,
                selection.roots_revision,
            )
            .await
            .map_err(invalid)?;
        let (_, target) = application
            .mcp
            .content_servers(
                &selection.registry_key,
                &selection.location.directory,
                &snapshot.declared,
            )
            .await
            .into_iter()
            .find(|(name, _)| name == &server)
            .ok_or_else(|| invalid("MCP server is absent or disabled"))?;
        let target = target.map_err(invalid)?;
        let value = match request {
            McpContentRequestDto::Resources {} => {
                let (resources, templates) = resource_catalog(&target).await.map_err(invalid)?;
                json!({"kind":"resources","location":selection.location.wire(),"catalog":{"resources":resources,"templates":templates}})
            }
            McpContentRequestDto::Prompt { name, arguments } => {
                let arguments = arguments
                    .as_map()
                    .iter()
                    .map(|(key, value)| (key.as_str().to_owned(), value.as_str().to_owned()))
                    .collect();
                let response = target
                    .prompt(name.as_str(), &arguments)
                    .await
                    .map_err(invalid)?;
                let content =
                    user_content::prompt(&server, name.as_str(), &response).map_err(invalid)?;
                json!({"kind":"user_content","location":selection.location.wire(),"content":content})
            }
            McpContentRequestDto::PinResource { uri, pinned } => {
                let session = selection
                    .session
                    .as_ref()
                    .ok_or_else(|| invalid("Select a Session before pinning a resource"))?;
                if pinned {
                    let interest = target
                        .subscribe(session, uri.as_str())
                        .await
                        .map_err(invalid)?;
                    selection.validate_current(&application).map_err(invalid)?;
                    interest.keep().map_err(invalid)?;
                } else {
                    target.unpin(session, uri.as_str());
                }
                target.changed();
                json!({"kind":"subscription", "location":selection.location.wire(),"notice":if pinned { "Resource pinned; updates appear in /mcp. Re-read explicitly to add new content." } else { "Resource unpinned." }})
            }
            McpContentRequestDto::ReadResource { uri } => {
                let interest = if target.subscribes {
                    let session = selection.session.as_ref().ok_or_else(|| {
                        invalid("Select a Session before reading a subscribable resource")
                    })?;
                    Some(
                        target
                            .subscribe(session, uri.as_str())
                            .await
                            .map_err(invalid)?,
                    )
                } else {
                    None
                };
                let revision = target.resource_revision(uri.as_str());
                let response = target.resource(uri.as_str()).await.map_err(invalid)?;
                let content =
                    user_content::resource(&server, uri.as_str(), &response).map_err(invalid)?;
                selection.validate_current(&application).map_err(invalid)?;
                if let Some(interest) = interest {
                    interest.read(revision);
                    interest.keep().map_err(invalid)?;
                    target.changed();
                }
                json!({"kind":"user_content","location":selection.location.wire(),"content":content})
            }
        };
        selection.validate_current(&application).map_err(invalid)?;
        decode(value).map_err(invalid)
    })
}

pub(crate) async fn resource_catalog(
    server: &super::mcp::content::ContentServer,
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let mut resources = server.catalog(CatalogKind::Resources).await?;
    let mut templates = server.catalog(CatalogKind::Templates).await?;
    for row in resources.iter_mut().chain(templates.iter_mut()) {
        row["server"] = json!(server.name);
    }
    Ok((resources, templates))
}

pub(crate) fn all_resources(
    application: ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<McpResourcesResponseDto, InvalidRequestErrorDto> {
    Box::pin(async move {
        let selection = session_config::catalog::select(&application, &query)
            .await
            .map_err(invalid)?;
        let snapshot =
            connections::McpSnapshot::read(&selection.sources, &selection.location.directory)
                .map_err(invalid)?;
        snapshot
            .prepare_roots(
                &application.mcp,
                &selection.registry_key,
                selection.roots_revision,
            )
            .await
            .map_err(invalid)?;
        let mut jobs = tokio::task::JoinSet::new();
        for (_, server) in application
            .mcp
            .content_servers(
                &selection.registry_key,
                &selection.location.directory,
                &snapshot.declared,
            )
            .await
        {
            let server = server.map_err(invalid)?;
            if server.resources {
                jobs.spawn(async move { resource_catalog(&server).await });
            }
        }
        let (mut resources, mut templates) = (Vec::new(), Vec::new());
        while let Some(result) = jobs.join_next().await {
            let (rows, patterns) = result.map_err(invalid)?.map_err(invalid)?;
            if resources.len() + rows.len() > 256 || templates.len() + patterns.len() > 256 {
                return Err(invalid(
                    "MCP resources exceed the aggregate 256-entry bound",
                ));
            }
            resources.extend(rows);
            templates.extend(patterns);
        }
        resources.sort_by_key(|row| row.to_string());
        templates.sort_by_key(|row| row.to_string());
        selection.validate_current(&application).map_err(invalid)?;
        decode(json!({"location":selection.location.wire(),"data":{"resources":resources,"templates":templates}})).map_err(invalid)
    })
}

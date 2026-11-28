use grapt_api::u3a::{
    ApplicationFuture, IntegrationListResponseDto, InvalidRequestErrorDto, McpAddRequestDto,
    McpApprovalPreviewDto, McpAuthRequestDto, McpAuthResponseDto, McpConfigurationListResponseDto,
    McpConfigurationRequestDto, McpContentRequestDto, McpContentResponseDto,
    McpInspectionResponseDto, McpListResponseDto, McpResourcesResponseDto, NoContent,
    ProjectCurrentLocationQueryDto,
};

use crate::dogfood::{connections, mcp, mcp_config, mcp_content, ServerApplication};

pub(super) fn integration_list(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<IntegrationListResponseDto, InvalidRequestErrorDto> {
    connections::integrations(application, query)
}

pub(super) fn mcp_inspect(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<McpInspectionResponseDto, InvalidRequestErrorDto> {
    connections::inspect(application, query)
}

pub(super) fn mcp_list(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<McpListResponseDto, InvalidRequestErrorDto> {
    connections::response(application.clone(), query, connections::Kind::McpServers)
}

pub(super) fn mcp_configuration_list(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<McpConfigurationListResponseDto, InvalidRequestErrorDto> {
    connections::configuration(application, query)
}

pub(super) fn mcp_add(
    application: &ServerApplication,
    server: String,
    query: ProjectCurrentLocationQueryDto,
    request: McpAddRequestDto,
) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
    connections::add_runtime(application.clone(), server, query, request)
}

pub(super) fn mcp_approval(
    application: &ServerApplication,
    server: String,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<McpApprovalPreviewDto, InvalidRequestErrorDto> {
    mcp_config::preview(application.clone(), server, query)
}

pub(super) fn mcp_configuration(
    application: &ServerApplication,
    server: String,
    query: ProjectCurrentLocationQueryDto,
    request: McpConfigurationRequestDto,
) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
    mcp_config::request(application.clone(), server, query, request)
}

pub(super) fn mcp_reconnect(
    application: &ServerApplication,
    server: String,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<NoContent, InvalidRequestErrorDto> {
    connections::reconnect(application.clone(), server, query)
}

pub(super) fn mcp_auth(
    application: &ServerApplication,
    server: String,
    query: ProjectCurrentLocationQueryDto,
    request: McpAuthRequestDto,
) -> ApplicationFuture<McpAuthResponseDto, InvalidRequestErrorDto> {
    mcp::auth_api::respond(application.clone(), server, query, request)
}

pub(super) fn mcp_content(
    application: &ServerApplication,
    server: String,
    query: ProjectCurrentLocationQueryDto,
    request: McpContentRequestDto,
) -> ApplicationFuture<McpContentResponseDto, InvalidRequestErrorDto> {
    mcp_content::respond(application.clone(), server, query, request)
}

pub(super) fn mcp_resources(
    application: &ServerApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<McpResourcesResponseDto, InvalidRequestErrorDto> {
    mcp_content::all_resources(application.clone(), query)
}

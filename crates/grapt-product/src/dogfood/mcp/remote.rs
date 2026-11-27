//! Registration glue for the bounded remote transport. Protocol/session,
//! admission, incarnation and call authority remain with the existing owners.

use super::*;

pub(super) async fn connect(
    registry: &McpRegistry,
    location: &str,
    server: &str,
    declaration: &Declaration,
    slot: ConnectionSlot,
    startup: Arc<super::startup_lease::StartupLease>,
    roots: Arc<super::roots::State>,
) -> ServerEntry {
    let Declaration::Remote {
        url,
        headers,
        oauth,
        timeouts,
    } = declaration
    else {
        return refused("remote MCP registration received a local declaration");
    };
    let Some(incarnation) = registry.next_incarnation() else {
        return refused("this process exhausted its MCP connection identities");
    };
    let client = match grapt_mcp_client::HttpClient::new_with_headers(
        url,
        registry.bounds.max_frame_bytes(),
        headers,
    ) {
        Ok(client) => client,
        Err(error) => return refused(&error.to_string()),
    };
    let client = if *oauth {
        let Some(auth) = &registry.auth else {
            return refused("MCP OAuth is unavailable in this composition");
        };
        let resource = match url::Url::parse(url) {
            Ok(url) => url.to_string(),
            Err(_) => return refused("invalid MCP resource URL"),
        };
        match client.with_bearer(auth.bearer(server, &resource)) {
            Ok(client) => client,
            Err(error) => return refused(&error.to_string()),
        }
    } else {
        client
    };
    let connection = match grapt_mcp_client::connect(
        client,
        grapt_mcp_client::TransportBudgets {
            startup: timeouts.startup,
            catalog: timeouts.catalog,
            execution: timeouts.execution,
        },
    ) {
        Ok(connection) => connection,
        Err(error) => return refused(&error.to_string()),
    };
    let grapt_mcp_client::RemoteConnection {
        reader,
        writer,
        task,
        human_wait,
    } = connection;
    let mut channel = McpChannel::new(
        Box::new(reader) as BoxedRead,
        Box::new(writer) as BoxedWrite,
        registry.bounds,
    )
    .with_human_wait(human_wait)
    .with_startup_lease(startup)
    .with_roots(roots.clone());
    let mut session = McpSession::declared(registry.session_bounds);
    let started = match registry
        .start_channel(&mut channel, &mut session, *timeouts, location, server)
        .await
    {
        Ok(started) => started,
        Err(failure) => return ServerEntry::unusable_in(failure.reason, failure.phase),
    };
    // The declaration snapshot binds the complete endpoint, including path/query.
    // The display label omits URL material that might contain credentials.
    let origin = match url::Url::parse(url) {
        Ok(url) => url.origin().ascii_serialization(),
        Err(_) => return refused("remote MCP endpoint changed during registration"),
    };
    ServerEntry::Live(Box::new(LiveServer {
        roots,
        notifications: None,
        stderr: None,
        identity: started.identity,
        catalog: started.catalog,
        timeouts: *timeouts,
        launch: LaunchIdentity {
            declaration: Arc::new(declaration.clone()),
            program: format!("remote:{origin}"),
            cwd: location.to_owned(),
            arguments: Vec::new(),
            incarnation,
        },
        io: Arc::new(HeldIo::new(LiveIo {
            _slot: slot,
            channel,
            session,
            _child: None,
            _stderr: None,
            _remote: Some(task),
        })),
    }))
}

fn refused(message: &str) -> ServerEntry {
    ServerEntry::unusable(Unusable::Failed(message.to_owned()))
}

#[cfg(test)]
mod tests;

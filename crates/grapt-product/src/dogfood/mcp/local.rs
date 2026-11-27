//! Construction of a held local server, including its process-owned diagnostics.
use super::{
    remote, spawn_local, BoxedRead, BoxedWrite, ConnectionSlot, Declaration, HeldIo,
    LaunchIdentity, LiveIo, LiveServer, McpChannel, McpRegistry, McpSession, ServerEntry, Unusable,
    MAX_LIVE_CONNECTIONS,
};
use std::{path::Path, sync::Arc};

impl McpRegistry {
    /// `location` is no longer read here: the declaration carries its cwd
    /// ALREADY RESOLVED against it, so the spawn uses that. Keeping the
    /// parameter would invite a future edit to spawn in the location while the
    /// identity named the resolved directory, which is precisely the
    /// disagreement the approval binding exists to prevent.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(crate) async fn connect_raw(
        &self,
        location: &str,
        server: &str,
        declaration: &Declaration,
        startup: Arc<super::startup_lease::StartupLease>,
        roots: Arc<super::roots::State>,
    ) -> ServerEntry {
        self.connect_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // ADMISSION. Refused BEFORE a process is spawned, because a bound
        // enforced after the fact is not a bound. Live and retired count the
        // same: a retired connection a caller still holds is still a child.
        let admitted = match ConnectionSlot::admit(&self.connections, MAX_LIVE_CONNECTIONS) {
            Some(slot) => slot,
            None => {
                return ServerEntry::unusable(Unusable::Failed(format!(
                    "this product already holds {MAX_LIVE_CONNECTIONS} MCP connections"
                )))
            }
        };
        let (command, cwd, environment, timeouts) = match declaration {
            Declaration::Disabled => return ServerEntry::Disabled,
            Declaration::Remote { .. } => {
                return remote::connect(
                    self,
                    location,
                    server,
                    declaration,
                    admitted,
                    startup,
                    roots,
                )
                .await
            }
            Declaration::Local {
                command,
                cwd,
                environment,
                timeouts,
            } => (command, cwd, environment, timeouts),
        };
        let Some(incarnation) = self.next_incarnation() else {
            return ServerEntry::unusable(Unusable::Failed(
                "this process exhausted its MCP connection identities".to_owned(),
            ));
        };
        // Spawned in the declaration's RESOLVED working directory. The bare
        // "." this used before meant a server ran in whatever directory the
        // product happened to be started from, which is neither the project a
        // user configured it for nor anything an approval could truthfully
        // name. `cwd` defaults to the location and is resolved against it, so
        // the default is unchanged and a configured value is honoured.
        let root = Path::new(cwd);
        let trace = super::startup_trace::StartupTrace::new(server, incarnation);
        let launched = match spawn_local(command, root, environment) {
            Ok(launched) => launched,
            Err(reason) => return ServerEntry::unusable(Unusable::Failed(reason)),
        };
        trace.mark("spawn");
        let (mut process, program) = launched;
        let child = &mut process.child;
        let capture = super::stderr::Capture::default();
        for value in environment.values().chain(command.iter().skip(1)) {
            capture.protect(value);
        }
        let Some(stderr) = child.stderr.take() else {
            return ServerEntry::unusable(Unusable::Failed(
                "the local MCP server exposed no stderr pipe".into(),
            ));
        };
        let reader = super::stderr::Reader::start(stderr, capture.clone());
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return ServerEntry::unusable(Unusable::Failed(
                "the local MCP server exposed no usable stdio".to_owned(),
            ));
        };
        let mut channel = McpChannel::new(
            Box::new(stdout) as BoxedRead,
            Box::new(stdin) as BoxedWrite,
            self.bounds,
        )
        .with_startup_trace(trace)
        .with_startup_lease(startup)
        .with_roots(roots.clone());
        let mut session = McpSession::declared(self.session_bounds);
        match self
            .start_channel(&mut channel, &mut session, *timeouts, location, server)
            .await
        {
            Ok(started) => ServerEntry::Live(Box::new(LiveServer {
                roots,
                notifications: None,
                timeouts: *timeouts,
                stderr: Some(capture),
                launch: LaunchIdentity {
                    declaration: Arc::new(declaration.clone()),
                    program,
                    // THE ACTUAL DIRECTORY, not the location. An approval binds
                    // the exact program, arguments and cwd; naming the location
                    // while the child ran somewhere else would bind an approval
                    // to a launch that never happened.
                    cwd: cwd.clone(),
                    arguments: command.iter().skip(1).cloned().collect(),
                    incarnation,
                },
                identity: started.identity,
                catalog: started.catalog,
                io: Arc::new(HeldIo::new(LiveIo {
                    _slot: admitted,
                    channel,
                    session,
                    _child: Some(process),
                    _stderr: Some(reader),
                    _remote: None,
                })),
            })),
            // The process is dropped here, and its group ends the whole tree: a
            // server that failed its startup does not keep running unattended.
            Err(failure) => {
                // Stop the whole tree first, so a descendant retaining stderr
                // cannot extend the reader's 250 ms drain grace.
                let _ = process.kill_tree();
                let tail = reader.finish().await;
                let reason = match failure.reason {
                    Unusable::Failed(detail) => Unusable::Failed(capture.sanitise(&detail)),
                    reason => reason,
                };
                ServerEntry::Unusable(reason, Some(failure.phase), tail)
            }
        }
    }
}

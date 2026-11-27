//! The transport adapter for one configured MCP server.
//!
//! Every decision about order, bounds and protocol shape already belongs to
//! `grapt-mcp`, which is pure. This module is the part that unavoidably
//! touches the operating system: it moves bytes over a duplex stream under a
//! deadline, and it starts a declared local server as a child process.
//!
//! It is generic over the stream so the framing can be exercised against an
//! in-memory duplex. A scripted server that answers late, answers twice,
//! answers with a giant frame or hangs up mid-reply is a fixture here; against
//! a real child process each of those is a race that has to be provoked.
//!
//! The child-process posture is deliberately the one this crate already uses
//! for the shell tool: a cleared environment with an explicit allowlist,
//! piped stdio, and `kill_on_drop`. The ratified Credentials row requires an
//! environment allowlist rather than an inherited environment, and a second
//! spawn posture in the same crate would be a parallel implementation of a
//! decision that already has an owner.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use grapt_mcp::session::{CallOutcome, McpSession, SessionBounds, Unusable};
use grapt_mcp::{
    accept_call, tools_call_request_with_metadata, DiscoveredTool, McpBounds, McpToolName,
    ServerIdentity, ToolCallOutcome,
};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

pub(super) mod auth;
pub(super) mod auth_api;
mod call;
mod call_lease;
pub(super) mod channel;
pub(super) mod content;
mod local;
pub(super) mod nested_startup;
pub(super) mod notifications;
mod producer_version;
mod remote;
pub(in crate::dogfood) mod roots;
mod startup;
use grapt_mcp_host::startup_trace;
pub(super) mod subscriptions;
pub(super) use startup::registry_with_forms;
#[cfg(test)]
mod startup_elicitation_tests;
use grapt_mcp_host::startup_lease;
#[cfg(test)]
use startup::start;
mod stderr;
pub(super) mod stderr_tail;
pub(super) use channel::{ChannelError, ConnectionSlot, HeldIo, McpChannel};

pub(crate) use grapt_mcp_host::startup::{StartFailure, StartPhase, StartedServer};

/// Run one tool call over a live channel, under this call's own lease.
///
/// Generic over the stream for the same reason `start` is: the races that
/// matter here — a duplicate call id, a reply after a disconnect, a lease
/// spent across a restart — are produced on demand against an in-memory duplex
/// and only rarely by a live child.
///
/// The lease is what makes those decidable rather than hopeful. `begin_call`
/// refuses a tool this connection never discovered, a call id already in
/// flight, a call id that already reached a terminal, and a load above the
/// declared bound — all before a byte is written. `settle` refuses a lease
/// minted under a retired generation, so a reply from an old connection
/// cannot settle a call on its replacement.
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is a distinct call coordinate; grouping them would               only move the same list"
)]
#[cfg(test)]
pub(super) async fn run_call<R, W>(
    channel: &mut McpChannel<R, W>,
    session: &mut McpSession,
    bounds: McpBounds,
    tool: &McpToolName,
    call_id: &str,
    arguments: Value,
    deadline: Duration,
) -> Result<ToolCallOutcome, McpCallRefusal>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    run_call_with(
        channel, session, bounds, tool, call_id, arguments, deadline, None, None, None, &mut false,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "existing call coordinates plus the nested request consumer"
)]
async fn run_call_with<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    channel: &mut McpChannel<R, W>,
    session: &mut McpSession,
    bounds: McpBounds,
    tool: &McpToolName,
    call_id: &str,
    arguments: Value,
    deadline: Duration,
    handler: Option<&mut dyn channel::ServerRequestHandler>,
    attempt: Option<&grapt_types::PhysicalAttemptId>,
    metadata: Option<Value>,
    channel_reusable: &mut bool,
) -> Result<ToolCallOutcome, McpCallRefusal> {
    // A local admission/refusal leaves the stream untouched. Once writing
    // starts, only a fully consumed reply makes it reusable again.
    *channel_reusable = true;
    let metadata = match handler.as_ref().and_then(|handler| handler.progress()) {
        Some(progress) => {
            let mut metadata = metadata.unwrap_or_else(|| serde_json::json!({}));
            let fields = metadata.as_object_mut().ok_or_else(|| {
                McpCallRefusal::Protocol("MCP request metadata is not an object".into())
            })?;
            fields.insert(
                "progressToken".into(),
                Value::String(progress.token().into()),
            );
            Some(metadata)
        }
        None => metadata,
    };
    let lease = session
        .begin_call(call_id, tool)
        .map_err(|error| McpCallRefusal::Session(error.to_string()))?;

    let generation = lease.generation().value();
    let lease = call_lease::CallLeaseGuard::new(session, lease);
    let request = match tools_call_request_with_metadata(
        channel.sequence(),
        bounds,
        tool,
        arguments,
        metadata,
    ) {
        Ok(request) => request,
        Err(error) => {
            // Settled even though nothing was sent: a call that never reached
            // the wire still reached a terminal, and leaving it in flight would
            // make a later attempt with the same id look like a duplicate.
            let _ = lease.finish(CallOutcome::Refused);
            return Err(McpCallRefusal::Protocol(error.to_string()));
        }
    };

    // The dispatch coordinates, taken from what this frame already holds rather
    // than reconstructed: the id this call was admitted under, the generation
    // that admission is valid for, and the ATTEMPT that admitted it. The first
    // two were already here; the attempt is the one identity that had to be
    // carried down from the loop that owns it, and it is what makes the pair
    // (call, attempt) select a single dispatch rather than a session.
    let dispatch =
        grapt_types::ToolCallId::new(call_id).map(|call| grapt_form::global::CallDispatch {
            call,
            generation,
            attempt: attempt.map(|attempt| attempt.as_str().to_owned()),
        });
    *channel_reusable = false;
    let replied = channel
        .exchange_with(request, deadline, handler, dispatch.as_ref())
        .await;
    *channel_reusable = replied.is_ok();
    let outcome = match &replied {
        Ok(_) => CallOutcome::Completed,
        Err(ChannelError::TimedOut) => CallOutcome::TimedOut,
        Err(ChannelError::Disconnected(_)) => CallOutcome::Disconnected,
        Err(ChannelError::Protocol(_)) => CallOutcome::Refused,
    };
    let settled = lease.finish(outcome);

    let result = replied.map_err(|error| match error {
        ChannelError::TimedOut => McpCallRefusal::TimedOut,
        ChannelError::Disconnected(detail) => McpCallRefusal::Disconnected(detail),
        ChannelError::Protocol(error) => McpCallRefusal::Protocol(error.to_string()),
    })?;
    settled.map_err(|error| McpCallRefusal::Session(error.to_string()))?;

    // A tool that RAN and reported failure is a completed call with an error
    // result, not a refusal: the caller continues from it. Only the wire being
    // unusable is a refusal here.
    accept_call(bounds, &result).map_err(|error| {
        let detail = match error {
            grapt_mcp::McpProtocolError::CallResultTooLarge { limit, .. } => {
                format!("MCP output exceeded {limit} bytes; use a smaller limit")
            }
            other => other.to_string(),
        };
        McpCallRefusal::Protocol(detail)
    })
}

mod spawn;
pub(super) use spawn::spawn_local;

mod entry;
pub(super) use entry::ServerEntry;
pub(super) use grapt_mcp_host::declaration::Declaration;

pub(super) use grapt_mcp_host::timeouts::McpTimeouts;

/// A held connection to one server.
/// Erased stream types, so a held connection can be backed by a child process
/// in production and by a duplex in a witness.
pub(super) type BoxedRead = Box<dyn AsyncRead + Unpin + Send>;
pub(super) type BoxedWrite = Box<dyn AsyncWrite + Unpin + Send>;

pub(super) struct LiveServer {
    roots: Arc<roots::State>,
    notifications: Option<notifications::Intake>,
    identity: ServerIdentity,
    stderr: Option<stderr::Capture>,
    /// This server's configured phase budgets, captured at connect.
    ///
    /// HELD ON THE CONNECTION, not passed by the caller. The execution budget
    /// belongs to the SERVER a user configured, so a caller supplying its own
    /// could dispatch against a server under a deadline that server's
    /// configuration never named — and two callers could then disagree about
    /// the same connection.
    timeouts: McpTimeouts,
    /// What actually ran, and under which connection identity.
    launch: LaunchIdentity,
    /// The bounded schemas this server published, kept because they are what a
    /// model is shown. Names alone cannot produce a callable tool definition.
    catalog: Vec<DiscoveredTool>,
    /// The mutable half, behind its OWN lock.
    ///
    /// A tool call may take up to `MCP_CALL_BUDGET` and a silent server will
    /// take all of it. Holding the registry's lock for that long would make one
    /// bad server block every other location's status read and every unrelated
    /// dispatch — the whole product serialized behind one connection. The
    /// registry lock is now held only long enough to clone this handle.
    io: Arc<HeldIo<LiveIo>>,
}

/// The parts of a connection that only one caller may touch at a time.
pub(super) struct LiveIo {
    /// Released when this connection is finally dropped, whoever was last to
    /// hold it. That is what makes the aggregate bound count processes rather
    /// than map entries.
    _slot: ConnectionSlot,
    pub(super) channel: McpChannel<BoxedRead, BoxedWrite>,
    pub(super) session: McpSession,
    /// Held so the child outlives the channel. Dropping this kills the server
    /// and every descendant, which is why the channel is never handed out
    /// separately. `None` for a connection with no spawned child, which only a
    /// witness has.
    _child: Option<spawn::LocalProcess>,
    _stderr: Option<stderr::Reader>,
    _remote: Option<grapt_mcp_client::RemoteTask>,
}

/// Hand-written because the erased streams cannot derive it. What a reader
/// wants from a held connection is who it is and what it launched, not the
/// state of a pipe.
impl std::fmt::Debug for LiveServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveServer")
            .field("identity", &self.identity)
            .field("launch", &self.launch)
            .field("tools", &self.catalog.len())
            .finish()
    }
}

impl LiveServer {
    pub(super) fn identity(&self) -> &ServerIdentity {
        &self.identity
    }

    pub(super) fn catalog(&self) -> &[DiscoveredTool] {
        &self.catalog
    }

    /// Whether a caller has OBSERVED this connection fail.
    ///
    /// A `Live` entry means "startup succeeded", which is a fact about the
    /// past. A hangup observed during a call is a fact about the present, and
    /// reporting the first while knowing the second is how a status list says
    /// connected about a dead pipe and advertises its tools.
    pub(super) fn observed_unusable(&self) -> Option<Unusable> {
        self.io.observed_unusable()
    }

    pub(super) fn launch(&self) -> &LaunchIdentity {
        &self.launch
    }
}

mod admission;
mod availability;
mod retention;
mod retry;
pub(super) use retention::{
    next_read_ordinal, release_over_bound, retire, MAX_LIVE_CONNECTIONS, MAX_QUEUED_CALLERS,
    MAX_RETAINED_LOCATIONS,
};

mod publication;
#[cfg(test)]
pub(super) use publication::MAX_PUBLISHED_SCHEMA_BYTES;
pub(super) use publication::{
    tool_definitions, tool_definitions_for_session, McpRoute, PublishedTools, WithheldTool,
};

mod approval_identity;
pub(super) use approval_identity::describe_launch;
pub(crate) use approval_identity::LaunchIdentity;

/// Why a tool call could not be run. Each variant is a different answer to
/// "did it happen?", which is the question a caller must not have to guess at:
/// a timeout says nothing about whether the server acted, while a refusal says
/// it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum McpCallRefusal {
    /// No server of that name is configured for that location.
    NoSuchServer(String),
    /// No connections are held for this LOCATION at all.
    ///
    /// Distinct from `NoSuchServer` because it points somewhere else entirely:
    /// at how the dispatch key was derived, not at the configuration.
    NoSuchLocation(String),
    /// The server is declared but not usable, with the reason it is not.
    NotConnected(Unusable),
    /// The connection refused the call before anything was sent: an
    /// undiscovered tool, a repeated call id, a retired generation, or the
    /// in-flight bound.
    Session(String),
    /// The deadline passed. NOTHING is known about whether the server ran it.
    TimedOut,
    /// The transport closed while the call was in flight.
    Disconnected(String),
    /// Bytes were exchanged and could not be used.
    Protocol(String),
    /// The bound producer rejected its older client. The request already
    /// crossed the wire and was not replayed.
    ProducerVersionChanged { recovered: bool },
    /// The replacement also refused after the single permitted read replay.
    ProducerReadRetryRefused,
    /// The replacement also refused after one send with its original key.
    ProducerWriteRetryRefused,
    /// The connection already has as many callers waiting as it admits.
    Busy { waiting: usize },
    /// The declaration changed, or the connection retired before completion.
    /// A same-declaration reconnect can only rebind a call before dispatch;
    /// an in-flight call is never replayed on the replacement.
    ConnectionReplaced { approved: String, current: String },
}

/// One location's held connections, and the exact declarations they were
/// started from.
///
/// The declarations are kept because a connection's identity is the pair: a
/// server named `github` started from one command is not the server named
/// `github` in a configuration that now names another.
#[derive(Debug, Default)]
pub(crate) struct LocationConnections {
    roots: Option<roots::Prepared>,
    pub(crate) declarations: BTreeMap<String, Declaration>,
    pub(crate) entries: BTreeMap<String, ServerEntry>,
    /// When this location was last read, for the release policy. A monotonic
    /// ordinal rather than a clock: eviction order must not depend on wall
    /// time, which can move.
    pub(crate) last_read: u64,
    pub(crate) last_read_ms: i64,
    /// This location serves a Session requiring StateEngine continuity.
    pub(crate) bound_producer: Option<std::sync::Weak<std::sync::atomic::AtomicBool>>,
    /// The execution drain holds the strong reference until its turn finishes.
    pub(crate) active: std::sync::Weak<()>,
    /// Which INSTANCE of this location this is. A released location that is
    /// recreated is a different instance, so a start begun under the old one
    /// cannot install into the new.
    pub(crate) incarnation: u64,
    /// Real placement, never the opaque Session/configuration registry key.
    pub(crate) directory: String,
    /// Servers whose startup is in flight, so concurrent readers coalesce onto
    /// one attempt instead of racing to spawn duplicates they discard.
    pub(crate) starting: BTreeMap<String, std::sync::Weak<startup_lease::StartupLease>>,
    backoff: BTreeMap<String, retry::Backoff>,
}

/// The product's bounded registry of configured MCP servers.
///
/// A healthy server is reused while its declaration matches, and its connection is
/// HELD, so a status read is a report on a live session rather than a fresh
/// process spawn. That distinction matters twice: an HTTP status poll must not
/// be able to fork a process per request, and "connected" must mean a channel
/// exists right now rather than that one once did.
#[derive(Debug)]
pub(super) struct McpRegistry {
    roots_authority: Arc<std::sync::Mutex<roots::Authority>>,
    resource_states: std::sync::Mutex<Vec<std::sync::Weak<subscriptions::State>>>,
    resources_closing: std::sync::atomic::AtomicBool,
    pub(super) catalog_emitter: Option<Arc<notifications::Emitter>>,
    pub(super) auth: Option<Arc<auth::AuthManager>>,
    pub(super) forms: Option<super::elicitation::FormProducer>,
    clock: Arc<dyn grapt_types::ClockAuthority + Send + Sync>,
    /// Process-lifetime declarations admitted through ordinary `mcp.add`.
    /// Kept outside the evictable connection map so releasing an idle
    /// location cannot erase a successful registration behind the ACP
    /// adapter's deduplication state. The aggregate is finitely bounded.
    runtime: tokio::sync::Mutex<BTreeMap<String, BTreeMap<String, Declaration>>>,
    /// Held connections, scoped BY LOCATION. MCP configuration is per-project,
    /// so one flat name-keyed map would let projects share and evict each
    /// other's servers.
    entries: tokio::sync::Mutex<BTreeMap<String, LocationConnections>>,
    changed: tokio::sync::Notify,
    bounds: McpBounds,
    session_bounds: SessionBounds,
    client_version: String,
    /// How many locations may retain connections at once.
    ///
    /// The map grew without limit: every project a user visited kept its
    /// servers alive for the life of the process. The bound is on LOCATIONS
    /// rather than connections because a location is what a user opens and
    /// closes, and evicting half a project's servers would leave it half
    /// working with no way to say so.
    retained_locations: usize,
    /// LIVE PLUS RETIRED connections, which is what actually bounds processes.
    ///
    /// The location map bounds retained LOCATIONS and nothing else. A retired
    /// connection is still a child and a channel until the last caller holding
    /// it lets go, so counting map entries counts the wrong thing: a burst of
    /// evictions with callers still queued leaves processes this registry no
    /// longer lists. Every connection increments this when it is created and
    /// decrements when its last holder drops it, so the count is of what
    /// EXISTS rather than of what is reachable.
    connections: Arc<std::sync::atomic::AtomicUsize>,
    /// The next connection identity to mint. Never reused within a process.
    incarnations: std::sync::atomic::AtomicU64,
    /// How many times this registry has ATTEMPTED a connection.
    ///
    /// Counts attempts, so "reused" and "silently evicted and restarted" can
    /// be told apart — both leave an entry under the same name.
    connect_attempts: std::sync::atomic::AtomicUsize,
}

impl McpRegistry {
    pub(super) fn new(
        bounds: McpBounds,
        session_bounds: SessionBounds,
        client_version: impl Into<String>,
        clock: Arc<dyn grapt_types::ClockAuthority + Send + Sync>,
    ) -> Self {
        Self {
            roots_authority: Arc::default(),
            resource_states: std::sync::Mutex::new(Vec::new()),
            resources_closing: std::sync::atomic::AtomicBool::new(false),
            auth: None,
            catalog_emitter: None,
            forms: None,
            clock,
            runtime: tokio::sync::Mutex::new(BTreeMap::new()),
            entries: tokio::sync::Mutex::new(BTreeMap::new()),
            changed: tokio::sync::Notify::new(),
            bounds,
            session_bounds,
            client_version: client_version.into(),
            retained_locations: MAX_RETAINED_LOCATIONS,
            connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            incarnations: std::sync::atomic::AtomicU64::new(1),
            connect_attempts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    #[cfg(test)]
    pub(super) fn connections_for_test(&self) -> &Arc<std::sync::atomic::AtomicUsize> {
        &self.connections
    }

    #[cfg(test)]
    pub(super) fn connect_attempts(&self) -> usize {
        self.connect_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Refresh declared servers, retrying failed local starts with bounded backoff; run
    /// `read` over the resulting entries.
    ///
    /// The closure form keeps the live sessions inside the lock: handing a
    /// `LiveServer` out would let a caller hold a channel while another caller
    /// restarted it, which is the shape that produces a use of a retired
    /// generation.
    #[cfg(test)]
    pub(super) async fn with_status<T>(
        &self,
        location: &str,
        declared: &BTreeMap<String, Declaration>,
        read: impl FnOnce(&BTreeMap<String, ServerEntry>) -> T,
    ) -> T {
        self.with_status_at(location, location, declared, read)
            .await
    }

    pub(super) async fn with_status_at<T>(
        &self,
        key: &str,
        directory: &str,
        declared: &BTreeMap<String, Declaration>,
        read: impl FnOnce(&BTreeMap<String, ServerEntry>) -> T,
    ) -> T {
        admission::with_status(self, key, directory, declared, false, None, read).await
    }

    /// Add or replace one process-lifetime declaration. Connecting remains in
    /// `with_status`, so runtime and configured servers share one admission,
    /// replacement, and connection-count path.
    pub(super) async fn register_runtime(
        &self,
        location: &str,
        name: String,
        declaration: Declaration,
    ) -> Result<(), String> {
        let mut runtime = self.runtime.lock().await;
        let existing = runtime
            .get(location)
            .and_then(|declarations| declarations.get(&name));
        let count = runtime.values().map(BTreeMap::len).sum::<usize>();
        if existing.is_none() && count >= 256 {
            return Err("runtime MCP catalog exceeds 256 servers".to_owned());
        }
        let changed = existing != Some(&declaration);
        runtime
            .entry(location.to_owned())
            .or_default()
            .insert(name.clone(), declaration);
        drop(runtime);
        if changed {
            if let Some(held) = self.entries.lock().await.get_mut(location) {
                if let Some(token) = held
                    .starting
                    .remove(&name)
                    .and_then(|token| token.upgrade())
                {
                    token.retire();
                }
                held.declarations.remove(&name);
                if let Some(entry) = held.entries.remove(&name) {
                    retire(&entry);
                }
            }
        }
        Ok(())
    }

    /// Retire only this location's named server. The next catalog read or
    /// admitted call uses ordinary startup; dispatched effects are not replayed.
    pub(super) async fn reconnect(
        &self,
        location: &str,
        server: &str,
        declared: &BTreeMap<String, Declaration>,
    ) -> Result<(), String> {
        let declaration = {
            let runtime = self.runtime.lock().await;
            runtime
                .get(location)
                .and_then(|servers| servers.get(server))
                .or_else(|| declared.get(server))
                .cloned()
        };
        if declaration.is_none() || matches!(declaration, Some(Declaration::Disabled)) {
            return Err("Reconnect requires an enabled MCP server at this location".into());
        }
        if let Some(held) = self.entries.lock().await.get_mut(location) {
            if let Some(token) = held
                .starting
                .remove(server)
                .and_then(|token| token.upgrade())
            {
                token.retire();
            }
            held.backoff.remove(server);
            held.declarations.remove(server);
            if let Some(entry) = held.entries.remove(server) {
                retire(&entry);
            }
        }
        self.changed.notify_waiters();
        Ok(())
    }

    pub(super) async fn retire_server(&self, server: &str, resource: &str) {
        let mut locations = self.entries.lock().await;
        for held in locations.values_mut() {
            let matches = held.declarations.get(server).is_some_and(|declaration|
                matches!(declaration, Declaration::Remote { url, .. } if url::Url::parse(url).is_ok_and(|url|url.as_str()==resource)));
            if matches || held.starting.contains_key(server) {
                // Fence starts already in flight, as well as queued callers.
                held.incarnation = next_read_ordinal();
                for token in held.starting.values().filter_map(std::sync::Weak::upgrade) {
                    token.retire();
                }
                held.starting.clear();
                held.declarations.remove(server);
                if let Some(entry) = held.entries.remove(server) {
                    retire(&entry);
                }
            }
        }
    }

    /// The configured execution budget for one live server.
    ///
    /// The budget belongs to the SERVER a user configured, so a caller must
    /// ask rather than supply its own. `None` when nothing live is held for
    /// that name: there is then no configured server to have a budget, and a
    /// caller that invents one would be dispatching under a deadline no
    /// configuration named.
    pub(super) async fn execution_budget(&self, location: &str, server: &str) -> Option<Duration> {
        let locations = self.entries.lock().await;
        match locations.get(location)?.entries.get(server)? {
            ServerEntry::Live(live) => Some(live.timeouts.execution),
            _ => None,
        }
    }

    /// Install a LIVE connection built over an in-memory duplex, exactly as
    /// `connect` would have installed a spawned one.
    ///
    /// This exists so a witness can reach `call` on a connection that really
    /// handshook and discovered. Without it the only reachable path was a
    /// server that fails to start, and a refusal reached through NotConnected
    /// proves nothing about the replacement check.
    #[cfg(test)]
    pub(super) async fn install_live_for_test(
        &self,
        location: &str,
        name: &str,
        declaration: Declaration,
        stream: tokio::io::DuplexStream,
        program: &str,
    ) -> u64 {
        let (read, write) = tokio::io::split(stream);
        let roots = self.roots_for(location, name, &declaration).await;
        let mut channel = McpChannel::new(
            Box::new(read) as BoxedRead,
            Box::new(write) as BoxedWrite,
            self.bounds,
        )
        .with_roots(roots.clone());
        let mut session = McpSession::declared(self.session_bounds);
        let mut handler = startup::RefuseStartupElicitation;
        let started = match startup::start_with(
            &mut channel,
            &mut session,
            self.bounds,
            // The test helper installs a connection without a declaration, so
            // it uses the ordinary defaults rather than a registry-wide budget
            // that production no longer consults.
            McpTimeouts::default().startup,
            McpTimeouts::default().catalog,
            &self.client_version,
            Some(&mut handler),
        )
        .await
        {
            Ok(started) => started,
            Err(reason) => panic!("the witness connection must start: {reason:?}"),
        };
        let incarnation = match self.next_incarnation() {
            Some(incarnation) => incarnation,
            None => panic!("the witness must be able to mint an identity"),
        };
        let entry = ServerEntry::Live(Box::new(LiveServer {
            roots,
            notifications: None,
            timeouts: McpTimeouts::default(),
            launch: LaunchIdentity {
                declaration: Arc::new(declaration.clone()),
                program: program.to_owned(),
                // FROM THE DECLARATION, exactly as `connect` derives it. This
                // used the LOCATION, so a fixture could not express a server
                // running anywhere but its project root -- and any test
                // reasoning about `cwd` was reasoning about the fixture.
                cwd: match &declaration {
                    Declaration::Local { cwd, .. } => cwd.clone(),
                    _ => location.to_owned(),
                },
                arguments: match &declaration {
                    Declaration::Local { command, .. } => command.iter().skip(1).cloned().collect(),
                    _ => Vec::new(),
                },
                incarnation,
            },
            stderr: None,
            identity: started.identity,
            catalog: started.catalog,
            io: Arc::new(HeldIo::new(LiveIo {
                _slot: match ConnectionSlot::admit(&self.connections, MAX_LIVE_CONNECTIONS) {
                    Some(slot) => slot,
                    None => panic!("the witness must be admitted"),
                },
                channel,
                session,
                _child: None,
                _stderr: None,
                _remote: None,
            })),
        }));
        let mut locations = self.entries.lock().await;
        let held = locations.entry(location.to_owned()).or_default();
        held.directory = location.to_owned();
        held.declarations.insert(name.to_owned(), declaration);
        if let Some(previous) = held.entries.insert(name.to_owned(), entry) {
            retire(&previous);
        }
        incarnation
    }

    #[cfg(test)]
    pub(super) async fn retained_location_count(&self) -> usize {
        self.entries.lock().await.len()
    }

    #[cfg(test)]
    pub(super) async fn retains_location(&self, location: &str) -> bool {
        self.entries.lock().await.contains_key(location)
    }

    /// The I/O handle a witness can hold, so a test owner can occupy a
    /// connection deliberately instead of hoping a cancelled task drops one.
    #[cfg(test)]
    pub(super) async fn io_handle_for_test(
        &self,
        location: &str,
        server: &str,
    ) -> Option<Arc<HeldIo<LiveIo>>> {
        let locations = self.entries.lock().await;
        match locations.get(location)?.entries.get(server)? {
            ServerEntry::Live(live) => Some(Arc::clone(&live.io)),
            _ => None,
        }
    }

    /// Mint the next connection identity.
    ///
    /// Exhaustion is CHECKED rather than wrapped: a wrapped counter would
    /// eventually hand a new connection the identity of a retired one, which
    /// is the exact confusion this exists to prevent. At one connection per
    /// nanosecond this would take longer than the age of the universe, so the
    /// refusal is unreachable in practice and present anyway, because the
    /// alternative is silently reusing an identity.
    fn next_incarnation(&self) -> Option<u64> {
        let mut current = self.incarnations.load(std::sync::atomic::Ordering::Relaxed);
        loop {
            let next = current.checked_add(1)?;
            match self.incarnations.compare_exchange_weak(
                current,
                next,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => return Some(current),
                Err(observed) => current = observed,
            }
        }
    }
}

/// The registry this product composes.
///
/// PHASE BUDGETS NOW COME FROM THE DECLARATION, so this registry no longer
/// carries one. The constant it held documented its own fixedness as a virtue
/// -- "a knob here would only let a slow server be made to look healthy" --
/// which the frozen contract contradicts by shipping exactly that knob.
/// `McpTimeouts::default()` is the single source of the unconfigured values,
/// so there is no second place for them to drift.
pub(super) fn registry(
    clock: Arc<dyn grapt_types::ClockAuthority + Send + Sync>,
) -> Result<McpRegistry, String> {
    let bounds = McpBounds::conservative().map_err(|error| error.to_string())?;
    let session_bounds = SessionBounds::conservative().map_err(|error| error.to_string())?;
    Ok(McpRegistry::new(
        bounds,
        session_bounds,
        env!("CARGO_PKG_VERSION"),
        clock,
    ))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod resolution_tests;

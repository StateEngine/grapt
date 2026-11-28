//! A protocol-loop fixture over real HTTP and the ordinary bridge frame queue.
use crate::backend::tests::read_gate::Gate;
use crate::backend::{serve, Backend, Command, Handoff, Update};
use crate::bridge::Bridge;
use crate::test_support::Profile;
use grapt_client::{
    BasicCredential, Bounds, Client, ContractDouble, FramePoll, HttpConnection, Operation,
    Transport, WireRequest,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};
pub(super) type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(super) struct Fixture {
    pub backend: Backend,
    pub model: grapt_client::wire::ModelRow,
    pub gate: Arc<Gate>,
    pub authority: Arc<AtomicBool>,
    pub catalog: Arc<AtomicBool>,
    pub ui_ready: mpsc::Receiver<()>,
    pub source: Arc<Mutex<ContractDouble>>,
    pub session: grapt_client::wire::SessionWireId,
    feed: Box<dyn Fn(FramePoll) -> std::result::Result<(), String> + Send>,
    _profile: Profile,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<std::result::Result<(), String>>>,
}

fn request(
    socket: &mut std::net::TcpStream,
    source: &Mutex<ContractDouble>,
    gate: &Gate,
) -> Result {
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut raw = Vec::new();
    let (head, size) = loop {
        let mut bytes = [0; 2048];
        let count = socket.read(&mut bytes)?;
        if count == 0 && raw.is_empty() {
            return Ok(());
        }
        if count == 0 || raw.len() > 65_536 {
            return Err("incomplete fixture request".into());
        }
        raw.extend_from_slice(&bytes[..count]);
        if let Some(at) = raw.windows(4).position(|part| part == b"\r\n\r\n") {
            let text = std::str::from_utf8(&raw[..at])?;
            let size = text
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .map(str::parse::<usize>)
                .transpose()?
                .unwrap_or(0);
            if raw.len() >= at + 4 + size {
                break (at, size);
            }
        }
    };
    let text = std::str::from_utf8(&raw[..head])?;
    let mut first = text
        .lines()
        .next()
        .ok_or("request line")?
        .split_whitespace();
    let method = first.next().ok_or("method")?;
    let url = url::Url::parse(&format!(
        "http://localhost{}",
        first.next().ok_or("target")?
    ))?;
    let session = url
        .path()
        .strip_prefix("/api/session/")
        .and_then(|tail| tail.split('/').next())
        .and_then(|id| grapt_client::wire::session_id(id).ok());
    let mut wire = Operation::ALL
        .iter()
        .find_map(|op| {
            let wire = WireRequest::new(
                *op,
                session.as_ref(),
                url.path_segments().and_then(|mut parts| parts.nth(4)),
            )
            .ok()?;
            (op.method().as_str() == method && wire.path() == url.path()).then_some(wire)
        })
        .ok_or_else(|| format!("unhandled fixture request: {method} {}", url.path()))?;
    for (key, value) in url.query_pairs() {
        wire = wire.with_query(&key, &value);
    }
    if size > 0 {
        wire = wire.with_body(raw[head + 4..head + 4 + size].to_vec());
    }
    let mut response = source.lock().map_err(|_| "source lock")?.call(&wire)?;
    if wire.operation() == Operation::MessageList && response.status == 200 {
        let page: grapt_api::u3a::SessionMessagesResponseDto =
            serde_json::from_slice(&response.body)?;
        let mut rows = page.data().get().as_slice().to_vec();
        with_tool(&mut rows)?;
        response.body = grapt_client::wire::encode_messages(rows, None)?;
    }
    if wire.operation() == Operation::SessionSnapshot && response.status == 200 {
        response.body = snapshot_with_tool(&response.body)?;
    }
    let allow_expired = gate.answer(wire.operation())?;
    reply(socket, response, allow_expired)
}

/// One mixed tool row beside the newest durable rows, as both reads serve it.
fn with_tool(rows: &mut Vec<grapt_api::u3a::SessionMessageInfoDto>) -> Result {
    if !rows.is_empty() {
        let tool = grapt_client::wire::assistant_tool_message_in(
            "msg_boundary_tool",
            "shell",
            "call_boundary_tool",
            grapt_client::wire::ToolStateDouble::Completed {
                input: "{}".into(),
                output: "fixture tool result".into(),
            },
        )?;
        rows.insert(1, tool);
    }
    Ok(())
}

fn snapshot_with_tool(body: &[u8]) -> Result<Vec<u8>> {
    use grapt_api::u3a::{decode_page, encode_page, BoundedVec, SessionSnapshotResponseDto};
    use grapt_schema::Required;
    let limits = grapt_client::wire::limits()?;
    let snapshot: SessionSnapshotResponseDto = decode_page(body, limits)?;
    let mut rows = snapshot.rows().get().as_slice().to_vec();
    with_tool(&mut rows)?;
    let rows = BoundedVec::try_new(rows).map_err(|error| error.to_string())?;
    let snapshot = SessionSnapshotResponseDto::new(
        Required::new(snapshot.revision().get().clone()),
        Required::new(rows),
        Required::new(snapshot.cursor().get().clone()),
        Required::new(snapshot.permissions().get().clone()),
        Required::new(snapshot.forms().get().clone()),
        Required::new(snapshot.execution().get().clone()),
        Required::new(snapshot.provisional().get().clone()),
    );
    Ok(encode_page(&snapshot, limits)?)
}

fn reply(
    socket: &mut std::net::TcpStream,
    response: grapt_client::WireResponse,
    allow_expired: bool,
) -> Result {
    let sent = write!(
        socket,
        "HTTP/1.1 {} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.body.len()
    )
    .and_then(|()| socket.write_all(&response.body));
    match sent {
        // Only a deliberately expired held request may have been abandoned.
        Err(error)
            if allow_expired
                && matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                ) =>
        {
            Ok(())
        }
        result => result.map_err(Into::into),
    }
}

fn service(
    listener: TcpListener,
    inner: Arc<Mutex<ContractDouble>>,
    stopping: Arc<AtomicBool>,
    gate: Arc<Gate>,
) -> std::thread::JoinHandle<std::result::Result<(), String>> {
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        while !stopping.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_nonblocking(false).map_err(|e| e.to_string())?;
                    if requests.len() >= 128 {
                        return Err("fixture request bound".into());
                    }
                    let source = inner.clone();
                    let gate = gate.clone();
                    requests.push(std::thread::spawn(move || {
                        request(&mut socket, &source, &gate).map_err(|e| e.to_string())
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        for request in requests {
            request.join().map_err(|_| "request worker panicked")??;
        }
        Ok(())
    })
}

impl Fixture {
    pub fn start() -> Result<Self> {
        Self::starting(false)
    }
    pub fn ready() -> Result<Self> {
        let fixture = Self::start()?;
        fixture.take_until(|update| update.prompt_ready)?;
        fixture.command(Command::Models)?;
        until(|| !fixture.backend.updates.vacant())?;
        Ok(fixture)
    }
    pub fn hint(&self, operation: Operation) {
        let hint = if operation == Operation::PermissionRequestList {
            &self.authority
        } else {
            &self.catalog
        };
        hint.store(true, Ordering::Release);
        let _ = self.backend.wake.try_send(());
    }
    pub fn paused() -> Result<Self> {
        Self::starting(true)
    }
    fn starting(paused: bool) -> Result<Self> {
        let profile = Profile::new()?;
        let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
        client.refresh_projects()?;
        let project = client.projects().first().ok_or("project")?.clone();
        client.select_project(&project)?;
        let agent = client.agents().first().ok_or("agent")?.id.clone();
        client.select_agent(&agent)?;
        let model = client.models().first().ok_or("model")?.clone();
        client.select_model(&model)?;
        let session = client.create_session()?;
        let mut double = std::mem::replace(client.peer_mut(), ContractDouble::new());
        // The subscription's own mode-negotiating connected frame.
        let connected = double.poll_frame()?;
        while !matches!(double.poll_frame()?, FramePoll::Idle) {}
        let source = Arc::new(Mutex::new(double));
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let rpc = HttpConnection::attach(
            listener.local_addr()?,
            BasicCredential::new("fixture", "local-only")?,
            Bounds::PRODUCTION,
        );
        let stop = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(Gate::default());
        let server = service(listener, source.clone(), stop.clone(), gate.clone());
        let (commands, received) = mpsc::channel();
        let updates = Handoff::default();
        let worker_updates = updates.clone();
        let (wake, ready) = mpsc::sync_channel(1);
        let (ui_wake, ui_ready) = mpsc::sync_channel(1);
        let (bridge, feed) = Bridge::test_event_feed(rpc, wake.clone());
        let authority = bridge.authority_dirty.clone();
        let catalog = bridge.sessions_dirty.clone();
        let worker_wake = wake.clone();
        // Settle the connected frame before attaching and before the test
        // produces a turn.
        feed(connected)?;
        let mut client = Client::new(bridge, Bounds::PRODUCTION);
        client.pump()?;
        let mut config = profile.config()?;
        config.session = Some(if paused {
            grapt_client::wire::session_id("ses_absent_boundary")?
        } else {
            session.clone()
        });
        // Like Backend::start, the protocol worker lives for the UI process.
        std::thread::spawn(move || {
            let result = serve(
                config,
                received,
                &worker_updates,
                ready,
                worker_wake,
                &ui_wake,
                client,
            );
            if let Err(message) = result {
                worker_updates.fail(crate::backend::Failure {
                    message,
                    unattached: false,
                });
            }
        });
        let fixture = Self {
            gate,
            authority,
            catalog,
            ui_ready,
            model,
            backend: Backend {
                commands,
                updates,
                wake,
            },
            source,
            session,
            feed: Box::new(feed),
            _profile: profile,
            stop,
            server: Some(server),
        };
        until(|| !fixture.backend.updates.vacant())?;
        Ok(fixture)
    }
    pub fn frame(&self, frame: FramePoll) -> Result {
        // An injected frame is served as the double serves its own.
        let frame = self.source.lock().map_err(|_| "source lock")?.serve(frame);
        (self.feed)(frame)?;
        Ok(())
    }
    pub fn turn(&self, reply: &str) -> Result<Vec<FramePoll>> {
        let mut source = self.source.lock().map_err(|_| "source lock")?;
        source.run_turn(&self.session, "boundary request", reply);
        let mut frames = Vec::new();
        loop {
            match source.poll_frame()? {
                FramePoll::Idle => return Ok(frames),
                frame => frames.push(frame),
            }
        }
    }
    pub fn command(&self, command: Command) -> Result<crate::backend::CommandId> {
        let envelope = self.backend.updates.envelope(command);
        let id = envelope.0;
        self.backend.updates.handed();
        self.backend.commands.send(envelope)?;
        let _ = self.backend.wake.try_send(());
        Ok(id)
    }
    pub fn outcomes(&self, count: usize) -> Result<Vec<crate::backend::Settled>> {
        until(|| self.backend.updates.intake() == 0 && self.backend.updates.pass_age().is_none())?;
        assert!(
            self.take()?.outcomes.is_empty(),
            "first view stayed pending"
        );
        let mut outcomes = Vec::new();
        let began = tokio::time::Instant::now();
        while outcomes.len() < count {
            outcomes.extend(self.take()?.outcomes);
            if began.elapsed() > Duration::from_secs(10) {
                return Err("command settlements did not complete".into());
            }
        }
        Ok(outcomes)
    }
    pub fn take(&self) -> Result<Update> {
        let mut result = None;
        until(|| {
            result = self.backend.take_update();
            result.is_some()
        })?;
        result
            .ok_or("update")?
            .map_err(|failure| failure.message.into())
    }
    pub fn take_until(&self, mut accept: impl FnMut(&Update) -> bool) -> Result<Update> {
        let began = tokio::time::Instant::now();
        loop {
            let update = self.take()?;
            if accept(&update) {
                return Ok(update);
            }
            if began.elapsed() > Duration::from_secs(10) {
                return Err(format!(
                    "view did not settle: {} {:?}",
                    update.status, update.observed_running
                )
                .into());
            }
        }
    }
    pub fn barrier(&self) -> Result {
        let (reply, received) = mpsc::sync_channel(1);
        crate::send(&self.backend, Command::Quit(reply))?;
        received.recv_timeout(Duration::from_secs(2))??;
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.gate.release();
        self.stop.store(true, Ordering::Release);
        if let Some(server) = self.server.take() {
            let result = server
                .join()
                .map_err(|_| "HTTP fixture panicked".to_owned())
                .and_then(|r| r);
            if let Err(error) = result {
                if std::thread::panicking() {
                    eprintln!("HTTP fixture: {error}");
                } else {
                    panic!("HTTP fixture: {error}");
                }
            }
        }
    }
}
pub(super) fn until(mut condition: impl FnMut() -> bool) -> Result {
    let started = tokio::time::Instant::now();
    while !condition() {
        if started.elapsed() > Duration::from_secs(10) {
            return Err("boundary did not progress".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

//! Bounded byte adapter for the existing MCP channel.
//!
//! Serialize outgoing POST acceptance, read at most two response streams, and
//! let the protocol owner handle correlation, nested requests and authority.

#[path = "bridge_notifications.rs"]
mod notifications;

use crate::{active_time::Deadline, HttpClient, HttpError, HttpReply, Incoming, ReconnectPolicy};
use serde_json::Value;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader, DuplexStream, ReadBuf, ReadHalf,
    WriteHalf,
};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

const PIPE_BYTES: usize = 64 * 1024;
const QUEUED_FRAMES: usize = 2;
const MAX_RESPONSE_STREAMS: usize = 2;
type Failure = Arc<Mutex<Option<String>>>;
type PendingPost =
    Pin<Box<dyn Future<Output = Result<(HttpReply, Value, Deadline, bool), HttpError>> + Send>>;

/// Phase budgets supplied by the existing MCP session owner.
#[derive(Debug, Clone, Copy)]
pub struct TransportBudgets {
    pub startup: Duration,
    pub catalog: Duration,
    pub execution: Duration,
}

impl TransportBudgets {
    fn for_message(self, message: &Value) -> Duration {
        match message["method"].as_str() {
            Some("initialize" | "notifications/initialized") => self.startup,
            Some("tools/list" | "resources/list" | "resources/templates/list" | "prompts/list") => {
                self.catalog
            }
            _ => self.execution,
        }
    }
}

pub struct RemoteConnection {
    pub reader: RemoteRead,
    pub writer: WriteHalf<DuplexStream>,
    pub task: RemoteTask,
    pub human_wait: crate::HumanWait,
}

impl std::fmt::Debug for RemoteConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteConnection").finish_non_exhaustive()
    }
}

/// Keep this guard with the owning MCP connection. Retirement/cancellation
/// aborts the actor; dropping its JoinSet aborts every response reader too.
#[derive(Debug)]
pub struct RemoteTask(JoinHandle<()>);

impl Drop for RemoteTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct RemoteRead {
    inner: ReadHalf<DuplexStream>,
    failure: Failure,
}

impl std::fmt::Debug for RemoteRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteRead").finish_non_exhaustive()
    }
}

impl AsyncRead for RemoteRead {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buffer) {
            Poll::Ready(Ok(())) if buffer.filled().len() == before && buffer.remaining() > 0 => {
                match self.failure.lock() {
                    Ok(failure) => match failure.as_ref() {
                        Some(message) => Poll::Ready(Err(io::Error::other(message.clone()))),
                        None => Poll::Ready(Ok(())),
                    },
                    Err(_) => Poll::Ready(Err(io::Error::other("MCP transport state unavailable"))),
                }
            }
            result => result,
        }
    }
}

pub fn connect(
    client: HttpClient,
    budgets: TransportBudgets,
) -> Result<RemoteConnection, HttpError> {
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| HttpError::RuntimeOrBudget)?;
    if [budgets.startup, budgets.catalog, budgets.execution]
        .iter()
        .any(|budget| budget.is_zero() || *budget > Duration::from_secs(24 * 3600))
    {
        return Err(HttpError::RuntimeOrBudget);
    }
    let (local, remote) = tokio::io::duplex(PIPE_BYTES);
    let (reader, writer) = tokio::io::split(local);
    let (remote_reader, mut remote_writer) = tokio::io::split(remote);
    let failure: Failure = Arc::new(Mutex::new(None));
    let destination = Arc::clone(&failure);
    let human_wait = client.human_wait.clone();
    let task = runtime.spawn(async move {
        // Keep the writer outside `run`: publish its safe error before EOF
        // wakes the reader, including when the two tasks use different threads.
        let outcome = run(client, remote_reader, &mut remote_writer, budgets).await;
        if let Err(error) = outcome {
            if let Ok(mut failure) = destination.lock() {
                *failure = Some(error.to_string());
            }
        }
    });
    Ok(RemoteConnection {
        reader: RemoteRead {
            inner: reader,
            failure,
        },
        writer,
        task: RemoteTask(task),
        human_wait,
    })
}

async fn run(
    client: HttpClient,
    reader: ReadHalf<DuplexStream>,
    writer: &mut WriteHalf<DuplexStream>,
    budgets: TransportBudgets,
) -> Result<(), HttpError> {
    let mut reader = BufReader::new(reader);
    let mut partial = Vec::new();
    let (outbound, mut received) = mpsc::channel::<Vec<u8>>(QUEUED_FRAMES);
    let mut streams = JoinSet::new();
    let mut post: Option<PendingPost> = None;
    let mut notifications = JoinSet::new();
    let mut notifications_started = false;
    loop {
        let can_read = post.is_none();
        let has_post = post.is_some();
        let has_streams = !streams.is_empty();
        let has_notifications = !notifications.is_empty();
        tokio::select! {
            input = read_frame(&mut reader, &mut partial, client.frame_limit()), if can_read => {
                let Some(frame) = input? else { return Ok(()); };
                let message: Value = serde_json::from_slice(&frame).map_err(|_| HttpError::Protocol)?;
                let id = message["id"].clone();
                let initialized = message["method"] == "notifications/initialized";
                let budget = budgets.for_message(&message);
                let client = client.clone();
                post = Some(Box::pin(async move {
                    let deadline = client.human_wait.deadline(budget)?;
                    let reply = client.post(&frame, budget).await?;
                    Ok((reply, id, deadline, initialized))
                }));
            }
            result = async {
                match post.as_mut() {
                    Some(pending) => pending.await,
                    None => std::future::pending().await,
                }
            }, if has_post => {
                post = None;
                let (reply, id, deadline, initialized) = result?;
                if initialized && !notifications_started {
                    notifications_started = true;
                    notifications.spawn(notifications::listen(client.clone(), outbound.clone(), budgets.catalog));
                }
                if let HttpReply::Messages(incoming) = reply {
                    // Finished tasks still occupy JoinSet entries until joined.
                    // Drain them before applying the active-stream bound.
                    while let Some(finished) = streams.try_join_next() {
                        finished.map_err(|_| HttpError::Transport)??;
                    }
                    if streams.len() >= MAX_RESPONSE_STREAMS {
                        return Err(HttpError::Protocol);
                    }
                    streams.spawn(read_response(client.clone(), *incoming, id, outbound.clone(), deadline));
                }
            }
            frame = received.recv() => {
                let Some(frame) = frame else { return Err(HttpError::Transport); };
                tokio::time::timeout(budgets.execution, async {
                    writer.write_all(&frame).await?;
                    writer.write_all(b"\n").await
                }).await.map_err(|_| HttpError::TimedOut)?
                    .map_err(|_| HttpError::Transport)?;
            }
            finished = notifications.join_next(), if has_notifications => {
                match finished { Some(Ok(result)) => result?, _ => return Err(HttpError::Transport) }
            }
            finished = streams.join_next(), if has_streams => {
                match finished {
                    Some(Ok(result)) => result?,
                    _ => return Err(HttpError::Transport),
                }
            }
        }
    }
}

/// The partial frame belongs to the actor, so cancelling this future at a
/// select boundary cannot discard bytes already consumed from the pipe.
async fn read_frame(
    reader: &mut BufReader<ReadHalf<DuplexStream>>,
    partial: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<Vec<u8>>, HttpError> {
    loop {
        let available = reader.fill_buf().await.map_err(|_| HttpError::Transport)?;
        if available.is_empty() {
            return if partial.is_empty() {
                Ok(None)
            } else {
                Err(HttpError::Protocol)
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(available.len());
        if take > limit.saturating_sub(partial.len()) {
            return Err(HttpError::Oversized);
        }
        partial.reserve_exact(take);
        partial.extend_from_slice(&available[..take]);
        reader.consume(take + usize::from(newline.is_some()));
        if newline.is_some() {
            if partial.is_empty() {
                return Err(HttpError::Protocol);
            }
            return Ok(Some(std::mem::take(partial)));
        }
    }
}

async fn read_response(
    client: HttpClient,
    mut incoming: Incoming,
    expected_id: Value,
    output: mpsc::Sender<Vec<u8>>,
    deadline: Deadline,
) -> Result<(), HttpError> {
    let mut stream_started = tokio::time::Instant::now();
    let mut policy = ReconnectPolicy::new();
    let mut cursor = None;
    let mut retry = None;
    loop {
        let remaining = deadline.remaining()?;
        let frame = incoming.next(remaining).await;
        if let Some(id) = incoming.last_event_id() {
            cursor = Some(id.to_owned());
        }
        retry = incoming.retry_delay().or(retry);
        match frame {
            Ok(Some(frame)) => {
                let message: Value =
                    serde_json::from_slice(&frame).map_err(|_| HttpError::Protocol)?;
                let terminal = message.get("id") == Some(&expected_id)
                    && (message.get("result").is_some() || message.get("error").is_some());
                deadline
                    .timeout(output.send(frame))
                    .await?
                    .map_err(|_| HttpError::Transport)?;
                if terminal {
                    return Ok(());
                }
                continue;
            }
            Ok(None)
            | Err(HttpError::Transport)
            | Err(HttpError::Sse(crate::SseDecodeError::Truncated)) => {}
            Err(error) => return Err(error),
        }
        let Some(cursor) = cursor.as_deref().filter(|cursor| !cursor.is_empty()) else {
            return Err(HttpError::Transport);
        };
        let mut lifetime = stream_started.elapsed();
        loop {
            let delay = policy
                .next_delay(lifetime, retry)
                .map_err(|_| HttpError::Transport)?;
            deadline.timeout(tokio::time::sleep(delay)).await?;
            let remaining = deadline.remaining()?;
            match client.resume(cursor, remaining).await {
                Ok(resumed) => {
                    incoming = resumed;
                    stream_started = tokio::time::Instant::now();
                    break;
                }
                Err(HttpError::Transport) | Err(HttpError::Status(500..=599)) => {
                    lifetime = Duration::ZERO
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests;

//! Duplex request dispatch, bounded concurrency, correlation, and shutdown.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Mutex, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

use super::wire::{FrameRead, FrameReader, Inbound, RpcFault, RpcId};
use super::AcpService;
use crate::CliLimits;

/// ACP output includes the complete JSON envelope and its trailing newline.
pub(crate) const FRAME_BYTES: usize = crate::bounds::MAX_STDIN_RECORD_BYTES;

const WRITE_ALLOWANCE: Duration = Duration::from_secs(30);
const CLIENT_REPLY_ALLOWANCE: Duration = Duration::from_secs(1_800);

#[derive(Debug, Error)]
pub enum AcpConnectionError {
    #[error("ACP stdin read failed: {0:?}")]
    Read(std::io::ErrorKind),
    #[error("ACP stdout write failed: {0:?}")]
    Write(std::io::ErrorKind),
    #[error("ACP output blocked beyond a finite allowance")]
    WriteTimeout,
    #[error("ACP response frame exceeded its finite {maximum}-byte bound")]
    OutputFrameTooLarge { maximum: usize },
    #[error("ACP bounded output queue closed")]
    OutputClosed,
    #[error("ACP client request id space is exhausted")]
    RequestIdExhausted,
    #[error("ACP client response exceeded its finite allowance")]
    ClientResponseTimeout,
    #[error("ACP request task failed")]
    Task,
}

enum Outbound {
    Message {
        value: Value,
        settled: oneshot::Sender<Result<(), AcpConnectionError>>,
        timing: Option<grapt_types::timing::TimingContext>,
    },
    Diagnostic(String),
}

type PendingReplies = Arc<Mutex<BTreeMap<RpcId, oneshot::Sender<Result<Value, RpcFault>>>>>;

enum RequestAdmission {
    Admitted(tokio::sync::OwnedSemaphorePermit),
    Duplicate,
    Busy,
}

#[derive(Clone)]
pub(crate) struct Peer {
    output: mpsc::Sender<Outbound>,
    pending: PendingReplies,
    next_id: Arc<AtomicU64>,
    frame_bytes: usize,
}

impl core::fmt::Debug for Peer {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Peer")
            .field("next_id", &self.next_id.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Peer {
    pub(crate) fn frame_limit(&self) -> usize {
        self.frame_bytes
    }

    async fn message(&self, value: Value) -> Result<(), AcpConnectionError> {
        self.message_with_timing(value, None).await
    }

    async fn message_with_timing(
        &self,
        value: Value,
        timing: Option<grapt_types::timing::TimingContext>,
    ) -> Result<(), AcpConnectionError> {
        let enqueue = timing
            .as_ref()
            .map(|context| context.span(grapt_types::timing::TimingStage::AcpEnqueue));
        let (settled, received) = oneshot::channel();
        tokio::time::timeout(
            WRITE_ALLOWANCE,
            self.output.send(Outbound::Message {
                value,
                settled,
                timing,
            }),
        )
        .await
        .map_err(|_| AcpConnectionError::WriteTimeout)?
        .map_err(|_| AcpConnectionError::OutputClosed)?;
        drop(enqueue);
        received
            .await
            .map_err(|_| AcpConnectionError::OutputClosed)?
    }

    pub(crate) fn diagnostic(&self, message: impl Into<String>) {
        let _queued = self.output.try_send(Outbound::Diagnostic(message.into()));
    }

    pub(crate) async fn notification(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<(), AcpConnectionError> {
        self.message(json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    pub(crate) async fn request(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<Value, RpcFault> {
        let ordinal = self
            .next_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| RpcFault::internal("ACP client request id space exhausted"))?;
        let id = RpcId::String(format!("grapt-{}", ordinal.saturating_add(1)));
        let (reply, received) = oneshot::channel();
        if self
            .pending
            .lock()
            .await
            .insert(id.clone(), reply)
            .is_some()
        {
            return Err(RpcFault::internal("ACP client request id collision"));
        }
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self.message(message).await.is_err() {
            self.pending.lock().await.remove(&id);
            return Err(RpcFault::internal("ACP output closed"));
        }
        match tokio::time::timeout(CLIENT_REPLY_ALLOWANCE, received).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcFault::internal("ACP client response channel closed")),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(RpcFault::internal("ACP client response timed out"))
            }
        }
    }

    async fn response(&self, id: &RpcId, result: Result<Value, RpcFault>) -> bool {
        self.pending
            .lock()
            .await
            .remove(id)
            .is_some_and(|waiter| waiter.send(result).is_ok())
    }

    async fn drain(&self) {
        let waiters = core::mem::take(&mut *self.pending.lock().await);
        for (_, waiter) in waiters {
            let _sent = waiter.send(Err(RpcFault::internal("ACP connection closed")));
        }
    }
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) async fn run_connection<R, W, E>(
    service: Arc<AcpService>,
    input: R,
    output: W,
    diagnostics: E,
    limits: CliLimits,
) -> Result<(), AcpConnectionError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    E: AsyncWrite + Unpin + Send + 'static,
{
    let (outbound, inbound) = mpsc::channel(limits.values().acp_inflight);
    let frame_bytes = limits.values().stdin_record_bytes.min(FRAME_BYTES);
    let peer = Peer {
        output: outbound,
        pending: Arc::new(Mutex::new(BTreeMap::new())),
        next_id: Arc::new(AtomicU64::new(0)),
        frame_bytes,
    };
    service.bind_peer(peer.clone())?;
    let mut writer = tokio::spawn(write_output(
        inbound,
        output,
        diagnostics,
        frame_bytes,
        Arc::clone(&service.timing),
    ));
    let semaphore = Arc::new(Semaphore::new(limits.values().acp_inflight));
    let active = Arc::new(Mutex::new(BTreeSet::new()));
    let mut tasks = JoinSet::new();
    let mut reader = FrameReader::new(input, limits);

    let result = loop {
        tokio::select! {
            biased;
            written = &mut writer => {
                break match written {
                    Ok(result) => result,
                    Err(_) => Err(AcpConnectionError::Task),
                };
            }
            completed = tasks.join_next(), if !tasks.is_empty() => {
                match completed {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(error))) => break Err(error),
                    Some(Err(_)) => break Err(AcpConnectionError::Task),
                    None => {}
                }
            }
            frame = reader.next() => {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(error) => break Err(AcpConnectionError::Read(error.kind())),
                };
                match frame {
                    FrameRead::Eof => break Ok(()),
                    FrameRead::TooLarge { maximum } => {
                        let fault = RpcFault::invalid_request(format!("ACP frame exceeds {maximum} bytes"));
                        if let Err(error) = peer.message(Inbound::failure(None, &fault)).await {
                            break Err(error);
                        }
                    }
                    FrameRead::Frame(frame) => match Inbound::decode(&frame) {
                        Ok(Inbound::Request { id, method, params }) => {
                            let permit = match admit_request(&active, &semaphore, &id).await {
                                RequestAdmission::Admitted(permit) => permit,
                                RequestAdmission::Duplicate => {
                                    let fault = RpcFault::invalid_request("duplicate active request id");
                                    if let Err(error) = peer.message(Inbound::failure(Some(&id), &fault)).await {
                                        break Err(error);
                                    }
                                    continue;
                                }
                                RequestAdmission::Busy => {
                                    let fault = RpcFault::server_busy(limits.values().acp_inflight);
                                    if let Err(error) = peer.message(Inbound::failure(Some(&id), &fault)).await {
                                        break Err(error);
                                    }
                                    continue;
                                }
                            };
                            let service = service.clone();
                            let peer = peer.clone();
                            let active = active.clone();
                            tasks.spawn(async move {
                                let _permit = permit;
                                let timing = if method == "session/prompt" {
                                    super::session::session_id(&params).ok().map(|session| service.timing.context(session))
                                } else { None };
                                let result = service.handle_request(&method, params).await;
                                let message = match result {
                                    Ok(result) => Inbound::success(&id, result),
                                    Err(fault) => Inbound::failure(Some(&id), &fault),
                                };
                                let written = peer.message_with_timing(message, timing).await;
                                remove_active(&active, &id).await;
                                written
                            });
                        }
                        Ok(Inbound::Notification { method, params }) => {
                            if let Err(fault) = service.handle_notification(&method, params).await {
                                peer.diagnostic(format!(
                                    "ACP notification refused: {} ({})",
                                    fault.message, fault.code
                                ));
                            }
                        }
                        Ok(Inbound::Response { id, result }) => {
                            if !peer.response(&id, result).await {
                                peer.diagnostic("ACP response carried an unknown or stale id");
                            }
                        }
                        Err((id, fault)) => {
                            if let Err(error) = peer.message(Inbound::failure(id.as_ref(), &fault)).await {
                                break Err(error);
                            }
                        }
                    },
                }
            }
        }
    };

    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    peer.drain().await;
    if !writer.is_finished() {
        writer.abort();
        let _writer_stopped = writer.await;
    }
    result
}

async fn write_output<W, E>(
    mut messages: mpsc::Receiver<Outbound>,
    mut output: W,
    diagnostics: E,
    maximum: usize,
    observer: Arc<super::timing::AcpTiming>,
) -> Result<(), AcpConnectionError>
where
    W: AsyncWrite + Unpin,
    E: AsyncWrite + Unpin + Send + 'static,
{
    let diagnostics =
        super::diagnostics::DiagnosticOutput::start(diagnostics, Arc::clone(&observer));
    while let Some(message) = messages.recv().await {
        match message {
            Outbound::Message {
                value,
                settled,
                timing,
            } => {
                let result = write_message(&mut output, &value, maximum, timing.as_ref()).await;
                let failed = result.is_err();
                let _settled = settled.send(result);
                let (samples, lost) = observer.drain();
                for sample in samples {
                    let diagnostic = format!("GRAPT_PROVIDER_DISPATCH clock=acp_monotonic tail_stage={:?} class={:?} session={} start_ms={} end_ms={} clock_order={}\n", sample.stage, sample.scope.class, sample.scope.session.as_ref().map_or("none", grapt_types::SessionId::as_str), sample.start.as_millis(), sample.end.as_millis(), if sample.end >= sample.start { "ordered" } else { "inverted" });
                    diagnostics.send(diagnostic);
                }
                if lost != 0 {
                    diagnostics.send(format!(
                        "GRAPT_PROVIDER_TAIL_LOSS acp_count_at_least={lost}"
                    ));
                }
                if failed {
                    return Err(AcpConnectionError::OutputClosed);
                }
            }
            Outbound::Diagnostic(message) => {
                diagnostics.send(message);
            }
        }
    }
    Ok(())
}

async fn write_message(
    output: &mut (impl AsyncWrite + Unpin),
    value: &Value,
    maximum: usize,
    timing: Option<&grapt_types::timing::TimingContext>,
) -> Result<(), AcpConnectionError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| AcpConnectionError::Task)?;
    bytes.push(b'\n');
    if bytes.len() > maximum {
        return Err(AcpConnectionError::OutputFrameTooLarge { maximum });
    }
    let write = timing.map(|context| context.span(grapt_types::timing::TimingStage::AcpWrite));
    tokio::time::timeout(WRITE_ALLOWANCE, output.write_all(&bytes))
        .await
        .map_err(|_| AcpConnectionError::WriteTimeout)?
        .map_err(|error| AcpConnectionError::Write(error.kind()))?;
    drop(write);
    let _flush = timing.map(|context| context.span(grapt_types::timing::TimingStage::AcpFlush));
    tokio::time::timeout(WRITE_ALLOWANCE, output.flush())
        .await
        .map_err(|_| AcpConnectionError::WriteTimeout)?
        .map_err(|error| AcpConnectionError::Write(error.kind()))
}

async fn insert_active(active: &Mutex<BTreeSet<RpcId>>, id: &RpcId) -> bool {
    active.lock().await.insert(id.clone())
}

async fn remove_active(active: &Mutex<BTreeSet<RpcId>>, id: &RpcId) {
    active.lock().await.remove(id);
}

async fn admit_request(
    active: &Arc<Mutex<BTreeSet<RpcId>>>,
    semaphore: &Arc<Semaphore>,
    id: &RpcId,
) -> RequestAdmission {
    if !insert_active(active, id).await {
        return RequestAdmission::Duplicate;
    }
    match semaphore.clone().try_acquire_owned() {
        Ok(permit) => RequestAdmission::Admitted(permit),
        Err(_) => {
            remove_active(active, id).await;
            RequestAdmission::Busy
        }
    }
}

#[allow(dead_code)]
fn _join_is_send<T: Send>(handle: JoinHandle<T>) -> JoinHandle<T> {
    handle
}

#[cfg(test)]
#[path = "transport_backpressure_tests.rs"]
mod backpressure_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[derive(Debug)]
    struct TickClock(std::sync::atomic::AtomicI64);
    impl grapt_types::ClockAuthority for TickClock {
        fn now(&self) -> grapt_types::Timestamp {
            grapt_types::Timestamp::from_millis(
                self.0.fetch_add(5, std::sync::atomic::Ordering::Relaxed),
            )
        }
    }

    #[tokio::test]
    async fn acp_transport_write_and_flush_keep_distinct_injected_boundaries(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use grapt_types::timing::TimingStage;
        let observer = super::super::timing::AcpTiming::bounded(Arc::new(TickClock(
            std::sync::atomic::AtomicI64::new(100),
        )));
        let context = observer.context("ses_transport_tail".parse()?);
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        let value = json!({"jsonrpc":"2.0", "id":1, "result":{"stopReason":"end_turn"}});
        write_message(&mut writer, &value, 1024, Some(&context)).await?;
        let mut expected = serde_json::to_vec(&value)?;
        expected.push(b'\n');
        let mut actual = vec![0; expected.len()];
        reader.read_exact(&mut actual).await?;
        assert_eq!(actual, expected, "timing must preserve the response frame");
        let (samples, lost) = observer.drain();
        assert_eq!(lost, 0);
        assert_eq!(samples.len(), 2);
        for (sample, stage, start, end) in [
            (&samples[0], TimingStage::AcpWrite, 100, 105),
            (&samples[1], TimingStage::AcpFlush, 110, 115),
        ] {
            assert_eq!(sample.stage, stage);
            assert_eq!(sample.scope, context.scope);
            assert_eq!(
                (sample.start.as_millis(), sample.end.as_millis()),
                (start, end)
            );
        }
        drop(reader);
        assert!(matches!(
            write_message(&mut writer, &value, 1024, Some(&context)).await,
            Err(AcpConnectionError::Write(_))
        ));
        let (samples, lost) = observer.drain();
        assert_eq!(lost, 0);
        assert_eq!(
            samples.len(),
            1,
            "failed write must settle its span without claiming a flush"
        );
        assert_eq!(samples[0].stage, TimingStage::AcpWrite);
        assert_eq!(
            (samples[0].start.as_millis(), samples[0].end.as_millis()),
            (120, 125)
        );
        Ok(())
    }

    #[tokio::test]
    async fn request_admission_refuses_duplicates_and_sheds_excess_concurrency() {
        let active = Arc::new(Mutex::new(BTreeSet::new()));
        let capacity = Arc::new(Semaphore::new(2));
        let first_id = RpcId::Signed(1);
        let second_id = RpcId::Signed(2);
        let third_id = RpcId::Signed(3);

        let RequestAdmission::Admitted(first) = admit_request(&active, &capacity, &first_id).await
        else {
            panic!("first request was not admitted");
        };
        assert!(
            matches!(
                admit_request(&active, &capacity, &first_id).await,
                RequestAdmission::Duplicate
            ),
            "duplicate active request id was admitted"
        );
        let RequestAdmission::Admitted(second) =
            admit_request(&active, &capacity, &second_id).await
        else {
            panic!("second request was not admitted");
        };
        assert!(
            matches!(
                admit_request(&active, &capacity, &third_id).await,
                RequestAdmission::Busy
            ),
            "excess concurrent request was not shed as busy"
        );
        assert!(!active.lock().await.contains(&third_id));

        drop(first);
        remove_active(&active, &first_id).await;
        assert!(matches!(
            admit_request(&active, &capacity, &third_id).await,
            RequestAdmission::Admitted(_)
        ));
        drop(second);
    }
}

#[cfg(test)]
#[path = "transport_frame_tests.rs"]
mod frame_tests;

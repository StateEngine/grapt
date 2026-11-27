//! Incremental durable recovery stays in the existing DTO controller. The
//! transport owns pending I/O; this module owns paging and bounded retry.

use super::*;
use std::time::Duration;

const RETRY_MIN: Duration = Duration::from_millis(250);
/// The longest wait between reconnect attempts; recovery after a restart
/// can take this long beyond the moment the service is ready again.
pub const RECONNECT_RETRY_MAX: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = RECONNECT_RETRY_MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Live,
    Reconnecting,
    Offline,
}

#[derive(Debug)]
pub(super) struct Recovery {
    pub(super) started: tokio::time::Instant,
    subscription_pending: bool,
    subscription_due: Duration,
    subscription_delay: Duration,
    pub(super) history: Option<History>,
    history_due: Duration,
    history_delay: Duration,
    /// The walk toward older history behind the installed tail, if any.
    pub(super) backfill: Option<super::history::Backfill>,
    /// How much history before the tail the selected session holds.
    pub(super) older: super::history::OlderHistory,
    /// Counts tail reads. A backfill reply carries the generation it was
    /// started under, so a restarted read of the same session fences it out.
    pub(super) generation: u64,
}

impl Default for Recovery {
    fn default() -> Self {
        Self {
            started: tokio::time::Instant::now(),
            subscription_pending: false,
            subscription_due: Duration::ZERO,
            subscription_delay: RETRY_MIN,
            history: None,
            history_due: Duration::ZERO,
            history_delay: RETRY_MIN,
            backfill: None,
            older: super::history::OlderHistory::default(),
            generation: 0,
        }
    }
}

/// A re-read after a terminal or a reconnect: the permission list, then the
/// newest page, installed as the tail the moment it decodes — a closed stream
/// can mean a cancelled ask, so the install carries the list read with it.
/// Older history is the background walk's, so a slow or refused older page
/// never holds the tail or the live frames behind it.
#[derive(Debug)]
pub(super) struct History {
    session: SessionWireId,
    permissions: bool,
    messages: bool,
    /// Which messages read is in flight; decided when it starts.
    read: MessagesRead,
    pub(super) pending: bool,
}

/// The legacy page read, or a snapshot on a positions connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessagesRead {
    Legacy,
    Snapshot,
}

impl ClientError {
    pub(crate) fn is_transient(&self) -> bool {
        match self {
            Self::Transport(error) => error.is_transient(),
            Self::Status { status, .. } => matches!(status, 408 | 429 | 500 | 502 | 503 | 504),
            _ => false,
        }
    }
}

impl<C: Connection> Client<C> {
    /// Sleep until recovery needs another pump; a healthy quiet client has no timer.
    pub fn recovery_wait(&self) -> Option<Duration> {
        self.recovery_wait_at(self.recovery.started.elapsed())
    }

    /// Uses the same monotonic epoch as `pump_at`.
    pub fn recovery_wait_at(&self, now: Duration) -> Option<Duration> {
        let poll = Duration::from_millis(10);
        let subscription = (!self.connection.subscribed()).then(|| {
            if self.recovery.subscription_pending {
                poll
            } else {
                self.recovery.subscription_due.saturating_sub(now)
            }
        });
        let history =
            if self.preset_pending() {
                Some(poll)
            } else if self.resume_pending() {
                // The handshake deadline is observed without spinning, and a
                // queued History is held, not due.
                Some(poll)
            } else if let Some(history) = &self.recovery.history {
                Some(if history.pending || self.waiting_for_mode() {
                    poll
                } else {
                    Duration::ZERO
                })
            } else if self.selected.is_some()
                && (!self.cursor.state().is_live() || !self.connection.subscribed())
            {
                Some(self.recovery.history_due.saturating_sub(now))
            } else {
                self.recovery.backfill.as_ref().map(|walk| {
                    if walk.pending {
                        poll
                    } else {
                        Duration::ZERO
                    }
                })
            };
        // Tool acquisition has its own incremental socket. Progress events may
        // stop before its reply arrives, so it must keep the quiet backend awake.
        let tools = self.tool_refresh.as_ref().map(|_| poll);
        let commands = self.commands_pending().then_some(poll);
        let catalog =
            (self.catalog_pending() || self.catalog_refresh.reading_repository()).then_some(poll);
        subscription
            .into_iter()
            .chain(history)
            .chain(tools)
            .chain(commands)
            .chain(catalog)
            .chain(self.operator_request_pending().then_some(poll))
            .chain(self.mcp_catalog_pending().then_some(poll))
            .min()
    }

    pub fn connection_status(&self) -> ConnectionStatus {
        if self.connection.subscribed() {
            ConnectionStatus::Live
        } else if self.recovery.subscription_pending {
            ConnectionStatus::Reconnecting
        } else {
            ConnectionStatus::Offline
        }
    }

    pub(super) fn recover(&mut self, now: Duration) -> Result<bool, ClientError> {
        let mut changed = false;
        if !self.connection.subscribed() {
            let result = if self.recovery.subscription_pending {
                Some(self.connection.poll_reconnect())
            } else if now >= self.recovery.subscription_due {
                // Each subscribe offers the frontier only while a resume is pending.
                self.offer_resume();
                Some(self.connection.begin_reconnect())
            } else {
                None
            };
            if let Some(result) = result {
                match result {
                    Ok(None) => self.recovery.subscription_pending = true,
                    Ok(Some(true)) => {
                        self.recovery.subscription_pending = false;
                        self.recovery.subscription_delay = RETRY_MIN;
                        changed = true;
                    }
                    Ok(Some(false)) => self.defer_subscription(now),
                    Err(error) if error.is_transient() => self.defer_subscription(now),
                    Err(error) => return Err(error.into()),
                }
            }
        }
        if self.preset_pending() {
            return Ok(changed);
        }
        if self
            .recovery
            .history
            .as_ref()
            .is_some_and(|h| Some(&h.session) != self.selected.as_ref())
        {
            self.connection.cancel_read();
            self.recovery.history = None;
            self.recovery.history_due = Duration::ZERO;
        }
        // A pending resume defers its history read until the first frame.
        if !self.history_blocks_stream()
            && (!self.cursor.state().is_live() || !self.connection.subscribed())
            && now >= self.recovery.history_due
            && !self.resume_pending()
        {
            self.request_history();
        }
        // A pending resume starts no queued History; the reconnect above
        // still runs, and the queued read starts once after the answer.
        if self.resume_pending() {
            return Ok(changed);
        }
        match self.advance_history(now) {
            Ok(updated) => changed |= updated,
            // This owner advances GET reads only. A reply lost after sending
            // one still needs bounded recovery even when its cursor was live.
            Err(error)
                if error.is_transient()
                    || matches!(
                        error,
                        ClientError::Transport(TransportError::AfterSend { .. })
                    ) =>
            {
                self.connection.cancel_read();
                self.recovery.history = None;
                self.recovery.history_due = now.saturating_add(self.recovery.history_delay);
                self.recovery.history_delay =
                    self.recovery.history_delay.saturating_mul(2).min(RETRY_MAX);
                self.cursor.stream_closed();
                changed = true;
            }
            Err(error) => return Err(error),
        }
        Ok(changed)
    }

    pub(super) fn request_history(&mut self) {
        if self.history_blocks_stream() {
            return;
        }
        if self
            .recovery
            .history
            .take()
            .is_some_and(|history| history.pending)
        {
            // A fresh input/terminal supersedes an older preview read.
            self.connection.cancel_read();
        }
        if let Some(session) = self.selected.clone() {
            self.begin_tail();
            self.recovery.history = Some(History {
                session,
                permissions: true,
                messages: true,
                read: MessagesRead::Legacy,
                pending: false,
            });
        }
    }

    pub(super) fn history_blocks_stream(&self) -> bool {
        self.recovery
            .history
            .as_ref()
            .is_some_and(|history| history.permissions || history.messages)
    }

    /// The queued messages read is waiting for the next stream to prove its
    /// mode, so that stream must be read until its first frame arrives.
    pub(super) fn waiting_for_mode(&self) -> bool {
        self.needs_mode_proof()
            && self
                .recovery
                .history
                .as_ref()
                .is_some_and(|history| !history.pending && !history.permissions && history.messages)
    }

    pub(super) fn defer_queued_inputs(&mut self) {
        if !self.history_blocks_stream()
            && self
                .recovery
                .history
                .take()
                .is_some_and(|history| history.pending)
        {
            self.connection.cancel_read();
        }
    }

    pub(super) fn request_queued_inputs(&mut self, session: &SessionWireId) {
        if self.selected.as_ref() == Some(session) {
            if self
                .recovery
                .history
                .as_ref()
                .is_some_and(|history| history.pending)
            {
                self.connection.cancel_read();
            }
            self.yield_backfill();
            // Optional presentation follows the installed tail on the existing
            // read lane, while live frames and the composer remain responsive.
            self.recovery.history = Some(History {
                session: session.clone(),
                permissions: false,
                messages: false,
                read: MessagesRead::Legacy,
                pending: false,
            });
        }
    }

    fn defer_subscription(&mut self, now: Duration) {
        self.recovery.subscription_pending = false;
        self.recovery.subscription_due = now.saturating_add(self.recovery.subscription_delay);
        self.recovery.subscription_delay = self
            .recovery
            .subscription_delay
            .saturating_mul(2)
            .min(RETRY_MAX);
    }

    fn advance_history(&mut self, now: Duration) -> Result<bool, ClientError> {
        // Embedded reads may finish immediately. Socket reads always yield
        // after starting one, so one iteration opens at most one read.
        for _ in 0..3 {
            let Some(mut history) = self.recovery.history.take() else {
                return Ok(false);
            };
            if !history.pending {
                history.read = if !history.permissions && history.messages {
                    // The messages read waits, bounded, for a stream to prove
                    // its mode; the deadline starts with the first wait.
                    let Some(read) = self.messages_read(Some(now)) else {
                        self.positions.handshake_since.get_or_insert(now);
                        self.recovery.history = Some(history);
                        return Ok(false);
                    };
                    read
                } else {
                    MessagesRead::Legacy
                };
            }
            let snapshot = history.read == MessagesRead::Snapshot;
            let operation = if history.permissions {
                Operation::PermissionRequestList
            } else if snapshot {
                Operation::SessionSnapshot
            } else if history.messages {
                Operation::MessageList
            } else {
                Operation::SessionPendingList
            };
            let response = if history.pending {
                self.connection.poll_read()
            } else {
                let request = if history.permissions {
                    WireRequest::new(operation, None, None)?
                } else if !history.messages || snapshot {
                    WireRequest::new(operation, Some(&history.session), None)?
                } else if self.history_window.is_some() {
                    super::paged_history::window_request(&history.session, None, true)?
                } else {
                    WireRequest::new(operation, Some(&history.session), None)?
                        .with_query("order", "desc")
                };
                self.connection.begin_read(&request)
            };
            let response = match response {
                Ok(response) => response,
                Err(error) if operation == Operation::SessionPendingList => {
                    self.connection.cancel_read();
                    self.queued_inputs_unavailable(&history.session, error.into());
                    return Ok(true);
                }
                Err(error) => return Err(error.into()),
            };
            let Some(response) = response else {
                history.pending = true;
                self.recovery.history = Some(history);
                return Ok(false);
            };
            history.pending = false;
            if operation == Operation::SessionPendingList {
                let result = self
                    .accept_response(operation, response)
                    .and_then(|bytes| self.install_queued_inputs(&history.session, &bytes));
                if let Err(error) = result {
                    self.queued_inputs_unavailable(&history.session, error);
                }
                return Ok(true);
            }
            let bytes = match self.accept_response(operation, response) {
                // This service has no snapshot: legacy mode for this connection.
                Err(error) if snapshot && super::snapshot::unsupported(&error) => {
                    // Kept queued: it reads once the legacy stream arrives.
                    self.recovery.history = Some(history);
                    self.refuse_snapshot();
                    return Ok(true);
                }
                accepted => accepted?,
            };
            if history.permissions {
                self.accept_permissions(bytes)?;
                history.permissions = false;
                self.recovery.history = Some(history);
                continue;
            }
            if history.messages {
                if snapshot {
                    self.install_snapshot(&history.session, &bytes)?;
                } else {
                    let page = crate::wire::decode_messages(&bytes)?;
                    self.adopt_tail(&history.session, &page, bytes.len())?;
                    self.positions.legacy_unproven |= self.positions.mode.is_none();
                }
                self.recovery.history_delay = RETRY_MIN;
                self.recovery.history_due = now.saturating_add(Duration::from_secs(1));
                history.messages = false;
                self.recovery.history = Some(history);
                continue;
            }
        }
        Ok(true)
    }
}

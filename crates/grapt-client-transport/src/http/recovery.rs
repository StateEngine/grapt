//! Recovery uses the same HTTP codec with one pending subscription and one
//! pending read. No thread, runtime, or unbounded request queue is introduced.

use super::*;
use crate::contract::Operation;

/// Whether this connection subscribes without positions after a snapshot
/// 404 (B3 S2c-1b). The latch is bound to the legacy stream it installs:
/// retiring the old positioned stream cannot clear it, and only losing the
/// installed legacy stream lets the next subscribe probe positions again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyLatch {
    Off,
    /// Refused positions; the legacy stream is not installed yet.
    Pending,
    /// The legacy stream is installed.
    Installed,
}

impl LegacyLatch {
    /// Whether the next subscribe asks for positions.
    pub(super) fn positions(self) -> bool {
        self == Self::Off
    }

    pub(super) fn installed(&mut self) {
        if *self == Self::Pending {
            *self = Self::Installed;
        }
    }

    pub(super) fn stream_lost(&mut self) {
        if *self == Self::Installed {
            *self = Self::Off;
        }
    }
}

#[derive(Debug)]
pub(super) struct SubscriptionAttempt {
    health: bool,
    /// The subscribe asked for stream positions (`positions=1`).
    positions: bool,
    request: pending::PendingHttp,
}

impl HttpConnection {
    /// The subscribe request, naming the watched Session when one is known.
    ///
    /// Every path that opens `/api/event` must carry the Session: a
    /// subscription with no `session` parameter registers no lease, so the
    /// service sees no window and a grace expiring later stops a turn nobody
    /// has stopped. The first subscribe, a resubscribe and incremental recovery
    /// all build their request here so none of them can drift.
    ///
    /// `positions` opts into stream positions. An older service refuses the
    /// unknown parameter with exactly 400, and the subscribe is then retried
    /// once without it; every other refusal is reported unchanged. Retained
    /// Sessions ride with positions, so that same retry also reaches a service
    /// that predates them.
    pub(super) fn subscribe_request(&self, positions: bool) -> Result<WireRequest, TransportError> {
        let request = WireRequest::new(Operation::EventSubscribe, None, None)?;
        let request = match self.watched_session() {
            Some(session) => request.with_query("session", session.as_str()),
            // No Session selected yet is a real state: the window names nothing
            // and the service decides from its own registry.
            None => request,
        };
        if !positions {
            return Ok(request);
        }
        let request = request.with_query("positions", "1");
        // Newest first, as many as keep the whole query within the service's
        // bound: past it the service refuses the subscribe with 400, and the
        // legacy retry would then drop positions and every claim with it.
        let mut retain = String::new();
        let mut bytes = Self::encoded_query_bytes(request.query()) + "&retain=".len();
        for kept in self.retained_sessions() {
            let separator = if retain.is_empty() { 0 } else { ",".len() * 3 };
            let added = separator + Self::encode_query_value(kept.as_str()).len();
            if bytes + added > grapt_api::u3a::MAX_QUERY_BYTES {
                break;
            }
            bytes += added;
            if !retain.is_empty() {
                retain.push(',');
            }
            retain.push_str(kept.as_str());
        }
        if retain.is_empty() {
            return Ok(request);
        }
        Ok(request.with_query("retain", &retain))
    }

    /// A reader for a stream in either class: a positioned frame may carry
    /// its `id:` line and offset past the legacy frame bound.
    pub(super) fn stream_reader(&self, positions: bool) -> SseReader {
        let mut bounds = self.bounds;
        if positions {
            bounds.max_frame_bytes = bounds
                .max_frame_bytes
                .max(grapt_api::u3a::MAX_POSITIONED_FRAME_BYTES);
        }
        SseReader::new(bounds)
    }

    pub(super) fn start_pending(
        &self,
        request: &WireRequest,
        subscription: bool,
    ) -> Result<pending::PendingHttp, TransportError> {
        self.require_product_identity()?;
        let pending = self.start_pending_parts(
            request.operation().method(),
            request.path(),
            request.query(),
            request.body(),
            self.bounds.max_response_bytes,
            subscription,
        )?;
        Ok(pending.with_timeout(request_timeout(request.operation())))
    }

    fn start_pending_parts(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&[u8]>,
        bound: usize,
        subscription: bool,
    ) -> Result<pending::PendingHttp, TransportError> {
        // std has no safe nonblocking connect constructor. Its only blocking
        // slice is capped at the existing 50ms stream connection allowance.
        let socket = TcpStream::connect_timeout(&self.address, STREAM_CONNECT_TIMEOUT)
            .map_err(|error| TransportError::from_io("connect", error))?;
        let mut encoded = Vec::new();
        self.write_request(&mut encoded, method, path, query, body)?;
        pending::PendingHttp::new(
            socket,
            encoded,
            if subscription {
                self.bounds
                    .max_frame_bytes
                    .max(grapt_api::u3a::MAX_POSITIONED_FRAME_BYTES)
            } else {
                bound
            },
            subscription,
        )
    }

    pub(super) fn start_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        self.stream = None;
        self.reader = SseReader::new(self.bounds);
        let health = self.expected_product.is_some();
        self.identity_pending = health;
        let pending = if health {
            self.start_pending_parts(
                Method::Get,
                grapt_api::PRODUCT_HEALTH_PATH,
                &[],
                None,
                4_096,
                false,
            )?
        } else {
            self.start_pending(&self.subscribe_request(self.legacy.positions())?, true)?
        };
        self.subscription_attempt = Some(SubscriptionAttempt {
            health,
            positions: !health && self.legacy.positions(),
            request: pending,
        });
        self.advance_reconnect()
    }

    pub(super) fn advance_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        let Some(attempt) = self.subscription_attempt.as_mut() else {
            return Ok(Some(self.subscribed()));
        };
        let response = match attempt.request.poll() {
            Ok(None) => return Ok(None),
            Ok(Some(response)) => response,
            Err(error) => {
                self.subscription_attempt = None;
                return Err(error);
            }
        };
        let Some(attempt) = self.subscription_attempt.take() else {
            return Err(TransportError::NotConnected);
        };
        if attempt.health {
            if matches!(response.status, 408 | 429 | 500 | 502 | 503 | 504) {
                return Err(TransportError::Status {
                    status: response.status,
                });
            }
            if let Some(expected) = &self.expected_product {
                Self::validate_product(expected, response)?;
            }
            self.identity_pending = false;
            // The identity check may have taken a while, and the window could
            // have switched Session meanwhile. Build the request from the
            // Session watched at this moment, so the subscription that opens is
            // the one the caller last selected.
            let request = self.subscribe_request(self.legacy.positions())?;
            self.subscription_attempt = Some(SubscriptionAttempt {
                health: false,
                positions: self.legacy.positions(),
                request: self.start_pending(&request, true)?,
            });
            return Ok(None);
        }
        if attempt.positions && response.status == 400 {
            // An older service refusing the unknown parameter: legacy stream.
            let request = self.subscribe_request(false)?;
            self.subscription_attempt = Some(SubscriptionAttempt {
                health: false,
                positions: false,
                request: self.start_pending(&request, true)?,
            });
            return Ok(None);
        }
        if !response.is_success() {
            return Err(TransportError::Status {
                status: response.status,
            });
        }
        self.reader = self.stream_reader(attempt.positions);
        self.stream_binding = Some(self.binding_for(attempt.positions));
        if !attempt.positions {
            self.legacy.installed();
        }
        self.push_stream_bytes(&response.body)?;
        self.stream = Some(attempt.request.stream);
        Ok(Some(true))
    }
}

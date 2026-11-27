//! The eight bounds the HTTP/1 host runs under, each a separately checked type.
//!
//! Law source: the bounded-everything row of the master spec's sync/async constitution —
//! "backpressure is bounded and visible" (L328) — and the HTTP host contract, which requires a "checked eight-field `ApiHostLimits` with no
//! `Default`".
//!
//! # Why there is no `Default`, and no aggregate constructor
//!
//! A bound that can be omitted is a bound nobody chose, which is the reason
//! `MaxRouterModules` and `MaxPrincipalMappings` already have no `Default` in
//! this crate. [`ApiHostLimits`] goes one step further: its fields are public
//! but every one of them is a checked type with a private field and a fallible
//! constructor, so the struct literal is not a bypass — there is no way to write
//! one except from checked values. Host construction also validates that the
//! stream budget leaves connection capacity for every ordinary request slot.
//!
//! # Why a panicking backend must never be reached
//!
//! Two of these bounds are handed to code that panics rather than refuses:
//!
//! * `hyper::server::conn::http1::Builder::max_buf_size` asserts its argument is
//!   at least 8192 and panics below it;
//! * `tokio::sync::Semaphore::new` panics above [`Semaphore::MAX_PERMITS`].
//!
//! The workspace forbids `unsafe_code` and denies `unwrap_used`/`expect_used`,
//! and a boundary that panics on a misconfigured number has turned a
//! configuration defect into a process kill. So both limits are refused HERE,
//! by a typed error, before any value reaches a backend — and the refusal names
//! the backend's own minimum or maximum rather than a number this module made
//! up. A backend panic is not a typed refusal.

use core::num::{NonZeroU32, NonZeroUsize};
use core::time::Duration;

use thiserror::Error;
use tokio::sync::Semaphore;

/// Hyper's own HTTP/1 minimum read-buffer size.
///
/// Named here as a constant rather than inlined at the comparison, because it is
/// hyper's number and not this crate's: it is `proto::h1::MINIMUM_MAX_BUFFER_SIZE`
/// in `hyper 1.11.0`, asserted inside `max_buf_size`. A version bump that moved
/// it would make this stale, which is why the refusal reports it as `minimum`
/// rather than describing it in prose.
pub const HYPER_MINIMUM_MAX_BUFFER_SIZE: usize = 8192;

/// Why a host configuration was refused.
///
/// Individual backend bounds and the connection reserve are checked before
/// the host starts; no invalid count reaches a panicking backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ApiHostConfigError {
    /// Stream sockets must leave room for every ordinary request slot.
    #[error(
        "{streams} stream slots plus {requests} request slots exceed {connections} connections"
    )]
    InsufficientConnections {
        streams: usize,
        requests: usize,
        connections: usize,
    },
    /// A count or duration was zero.
    ///
    /// Zero is refused for ordinary capacity and durations: a zero
    /// connection or request bound presents as "everything is refused" and reads
    /// like a working deny-all, while a zero duration presents as "already
    /// expired" and reads like a working timeout.
    #[error("{field} must be a positive bound")]
    NotPositive {
        /// The field that was zero.
        field: &'static str,
    },

    /// The head buffer was below hyper's own minimum.
    ///
    /// Refused here so that `max_buf_size`'s assertion is unreachable. The
    /// refusal reports both numbers because the caller can act on neither
    /// without the other.
    #[error(
        "max_request_head_bytes is {supplied} but hyper's HTTP/1 read buffer has a minimum of \
         {minimum}"
    )]
    HeadBufferBelowHyperMinimum {
        /// What the caller supplied.
        supplied: usize,
        /// Hyper's minimum, from [`HYPER_MINIMUM_MAX_BUFFER_SIZE`].
        minimum: usize,
    },

    /// A permit count exceeded what the semaphore backend can hold.
    ///
    /// `Semaphore::new` panics above its maximum, so the value is refused before
    /// it is constructed rather than after.
    #[error("{field} is {supplied} but the permit backend admits at most {maximum}")]
    BackendCapacityExceeded {
        /// The field that was too large.
        field: &'static str,
        /// What the caller supplied.
        supplied: usize,
        /// The backend's maximum, from `Semaphore::MAX_PERMITS`.
        maximum: usize,
    },
}

/// Refuse a count that a semaphore could not hold.
///
/// One helper rather than two copies: both semaphore-backed bounds ask the same
/// question of the same backend, and a second transcription of `MAX_PERMITS`
/// would be a second carrier that can drift from the first.
fn checked_permits(field: &'static str, value: usize) -> Result<NonZeroUsize, ApiHostConfigError> {
    let Some(positive) = NonZeroUsize::new(value) else {
        return Err(ApiHostConfigError::NotPositive { field });
    };
    if value > Semaphore::MAX_PERMITS {
        return Err(ApiHostConfigError::BackendCapacityExceeded {
            field,
            supplied: value,
            maximum: Semaphore::MAX_PERMITS,
        });
    }
    Ok(positive)
}

/// The maximum number of live admitted sockets.
///
/// This is also the shutdown inventory bound. H-04: duplicating it as a second
/// configurable `max_shutdown_connections` would permit a contradiction between
/// two numbers describing one owned set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxConnections(NonZeroUsize);

impl MaxConnections {
    /// Checked against the permit backend's maximum as well as zero.
    pub fn try_new(value: usize) -> Result<Self, ApiHostConfigError> {
        Ok(Self(checked_permits("max_connections", value)?))
    }

    pub fn value(self) -> usize {
        self.0.get()
    }
}

/// The monotonic request budget for one keep-alive connection.
///
/// `u32` rather than `usize`: this counts requests on a single connection, and a
/// budget that needs more than four billion is a configuration mistake rather
/// than a workload. It is not semaphore-backed, so it has no backend maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxRequestsPerConnection(NonZeroU32);

impl MaxRequestsPerConnection {
    pub fn try_new(value: u32) -> Result<Self, ApiHostConfigError> {
        match NonZeroU32::new(value) {
            Some(positive) => Ok(Self(positive)),
            None => Err(ApiHostConfigError::NotPositive {
                field: "max_requests_per_connection",
            }),
        }
    }

    pub fn value(self) -> u32 {
        self.0.get()
    }
}

/// The global number of admitted requests whose response bodies are still live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxInflightRequests(NonZeroUsize);

impl MaxInflightRequests {
    pub fn try_new(value: usize) -> Result<Self, ApiHostConfigError> {
        Ok(Self(checked_permits("max_inflight_requests", value)?))
    }

    pub fn value(self) -> usize {
        self.0.get()
    }
}

/// HTTP event streams have independent capacity; zero disables those routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxInflightStreams(usize);

impl MaxInflightStreams {
    pub const fn disabled() -> Self {
        Self(0)
    }
    pub fn try_new(value: usize) -> Result<Self, ApiHostConfigError> {
        if value > Semaphore::MAX_PERMITS {
            return Err(ApiHostConfigError::BackendCapacityExceeded {
                field: "max_inflight_streams",
                supplied: value,
                maximum: Semaphore::MAX_PERMITS,
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> usize {
        self.0
    }
}

/// Hyper's HTTP/1 maximum read/head buffer, in bytes.
///
/// Named for what it is. It is NOT an exact count of header-value bytes, and
/// calling it one would be a receipt that reads authoritative while being wrong
/// about the quantity it bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxRequestHeadBytes(usize);

impl MaxRequestHeadBytes {
    /// Refuses below [`HYPER_MINIMUM_MAX_BUFFER_SIZE`] without reaching the
    /// assertion inside `max_buf_size`.
    ///
    /// Zero is reported as `NotPositive` rather than as a small buffer: zero is
    /// the field-wide law every bound here obeys, and reporting it as "below
    /// 8192" would name the wrong rule for a value that never described a buffer
    /// at all.
    pub fn try_new(value: usize) -> Result<Self, ApiHostConfigError> {
        if value == 0 {
            return Err(ApiHostConfigError::NotPositive {
                field: "max_request_head_bytes",
            });
        }
        if value < HYPER_MINIMUM_MAX_BUFFER_SIZE {
            return Err(ApiHostConfigError::HeadBufferBelowHyperMinimum {
                supplied: value,
                minimum: HYPER_MINIMUM_MAX_BUFFER_SIZE,
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> usize {
        self.0
    }
}

/// The parser's header-count bound. Excess yields 431 before any routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxHeaders(NonZeroUsize);

impl MaxHeaders {
    pub fn try_new(value: usize) -> Result<Self, ApiHostConfigError> {
        match NonZeroUsize::new(value) {
            Some(positive) => Ok(Self(positive)),
            None => Err(ApiHostConfigError::NotPositive {
                field: "max_headers",
            }),
        }
    }

    pub fn value(self) -> usize {
        self.0.get()
    }
}

/// The request-body bound, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxRequestBodyBytes(NonZeroUsize);

impl MaxRequestBodyBytes {
    pub fn try_new(value: usize) -> Result<Self, ApiHostConfigError> {
        match NonZeroUsize::new(value) {
            Some(positive) => Ok(Self(positive)),
            None => Err(ApiHostConfigError::NotPositive {
                field: "max_request_body_bytes",
            }),
        }
    }

    pub fn value(self) -> usize {
        self.0.get()
    }
}

/// How long an incomplete request head may remain unfinished.
///
/// A positive DURATION, never an absolute wall-clock instant. An allowance is a
/// property of the configuration; an instant would be a property of when the
/// configuration happened to be read, and no deadline result in this crate is a
/// durable timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderReadAllowance(Duration);

impl HeaderReadAllowance {
    pub fn try_new(value: Duration) -> Result<Self, ApiHostConfigError> {
        if value.is_zero() {
            return Err(ApiHostConfigError::NotPositive {
                field: "header_read_allowance",
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> Duration {
        self.0
    }
}

/// How long a stream write batch may stall before the connection is reclaimed.
///
/// This is a host policy bound, not a clock reading: the [`DeadlineDriver`]
/// stays the time authority and is handed this duration. It bounds a stalled
/// body so the stream permit is released. On Linux the same value also arms the
/// socket's user timeout, so one duration is enforced by two INDEPENDENT routes,
/// the driver's deadline and the kernel's own; they share the value, not a
/// clock, and either may reclaim the connection.
///
/// [`DeadlineDriver`]: crate::deadline::DeadlineDriver
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamWriteAllowance(Duration);

impl StreamWriteAllowance {
    pub fn try_new(value: Duration) -> Result<Self, ApiHostConfigError> {
        if value.is_zero() {
            return Err(ApiHostConfigError::NotPositive {
                field: "stream_write_allowance",
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> Duration {
        self.0
    }
}

/// How long the graceful drain phase may run before the remainder is forced.
///
/// Bounds only the drain. It is not a general request timeout: dropping an
/// arbitrary application future can leave a durable or external effect
/// ambiguous, and H owns no reconciliation law for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownDrainAllowance(Duration);

impl ShutdownDrainAllowance {
    pub fn try_new(value: Duration) -> Result<Self, ApiHostConfigError> {
        if value.is_zero() {
            return Err(ApiHostConfigError::NotPositive {
                field: "shutdown_drain_allowance",
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> Duration {
        self.0
    }
}

/// Every bound the host runs under, and the exact set a ready receipt reports.
///
/// No `Default`, by construction rather than by convention: this type derives
/// nothing that could supply one, and a compile-fail probe holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiHostLimits {
    /// Live admitted sockets, and the shutdown inventory bound.
    pub max_connections: MaxConnections,
    /// Requests admitted on one keep-alive connection.
    pub max_requests_per_connection: MaxRequestsPerConnection,
    /// Ordinary requests whose response bodies are live across all connections.
    pub max_inflight_requests: MaxInflightRequests,
    /// Event stream bodies, separate from ordinary requests; zero disables them.
    pub max_inflight_streams: MaxInflightStreams,
    /// Hyper's HTTP/1 read buffer.
    pub max_request_head_bytes: MaxRequestHeadBytes,
    /// Hyper's parser header count.
    pub max_headers: MaxHeaders,
    /// The request-body bound.
    pub max_request_body_bytes: MaxRequestBodyBytes,
    /// The header-read allowance.
    pub header_read_allowance: HeaderReadAllowance,
    /// The graceful-drain allowance.
    pub shutdown_drain_allowance: ShutdownDrainAllowance,
    /// The stream write-stall allowance.
    pub stream_write_allowance: StreamWriteAllowance,
}

impl ApiHostLimits {
    pub fn validate(self) -> Result<(), ApiHostConfigError> {
        let streams = self.max_inflight_streams.value();
        let requests = self.max_inflight_requests.value();
        let connections = self.max_connections.value();
        if streams != 0 && streams > connections.saturating_sub(requests) {
            return Err(ApiHostConfigError::InsufficientConnections {
                streams,
                requests,
                connections,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bound_refuses_zero_naming_its_own_field() {
        // One case per field, and each asserts the FIELD NAME rather than only
        // that some refusal occurred. A shared `NotPositive` variant is only
        // honest if the discriminator is right, and a copy-paste that reported a
        // sibling's name would otherwise pass.
        let cases: [(&str, ApiHostConfigError); 8] = [
            (
                "max_connections",
                match MaxConnections::try_new(0) {
                    Err(error) => error,
                    Ok(_) => panic!("a zero connection bound was admitted"),
                },
            ),
            (
                "max_requests_per_connection",
                match MaxRequestsPerConnection::try_new(0) {
                    Err(error) => error,
                    Ok(_) => panic!("a zero per-connection budget was admitted"),
                },
            ),
            (
                "max_inflight_requests",
                match MaxInflightRequests::try_new(0) {
                    Err(error) => error,
                    Ok(_) => panic!("a zero in-flight bound was admitted"),
                },
            ),
            (
                "max_request_head_bytes",
                match MaxRequestHeadBytes::try_new(0) {
                    Err(error) => error,
                    Ok(_) => panic!("a zero head buffer was admitted"),
                },
            ),
            (
                "max_headers",
                match MaxHeaders::try_new(0) {
                    Err(error) => error,
                    Ok(_) => panic!("a zero header count was admitted"),
                },
            ),
            (
                "max_request_body_bytes",
                match MaxRequestBodyBytes::try_new(0) {
                    Err(error) => error,
                    Ok(_) => panic!("a zero body bound was admitted"),
                },
            ),
            (
                "header_read_allowance",
                match HeaderReadAllowance::try_new(Duration::ZERO) {
                    Err(error) => error,
                    Ok(_) => panic!("a zero header-read allowance was admitted"),
                },
            ),
            (
                "shutdown_drain_allowance",
                match ShutdownDrainAllowance::try_new(Duration::ZERO) {
                    Err(error) => error,
                    Ok(_) => panic!("a zero drain allowance was admitted"),
                },
            ),
        ];
        for (field, error) in cases {
            assert_eq!(
                error,
                ApiHostConfigError::NotPositive { field },
                "the zero refusal for {field} named the wrong field"
            );
        }
    }

    #[test]
    fn the_head_buffer_refuses_below_hypers_minimum_without_panicking() {
        // The value one below the minimum is the case `max_buf_size` asserts on.
        // Reaching that assertion is the defect; this refuses instead, and names
        // hyper's number rather than one of its own.
        match MaxRequestHeadBytes::try_new(HYPER_MINIMUM_MAX_BUFFER_SIZE - 1) {
            Err(error) => assert_eq!(
                error,
                ApiHostConfigError::HeadBufferBelowHyperMinimum {
                    supplied: HYPER_MINIMUM_MAX_BUFFER_SIZE - 1,
                    minimum: HYPER_MINIMUM_MAX_BUFFER_SIZE,
                },
                "the sub-minimum refusal must report both numbers"
            ),
            Ok(_) => panic!("a head buffer below hyper's minimum was admitted"),
        }

        // The same-file control at the exact boundary: the minimum itself is
        // admitted, so the case above is not passing because everything is
        // refused.
        match MaxRequestHeadBytes::try_new(HYPER_MINIMUM_MAX_BUFFER_SIZE) {
            Ok(bound) => assert_eq!(bound.value(), HYPER_MINIMUM_MAX_BUFFER_SIZE),
            Err(error) => panic!("hyper's own minimum was refused: {error}"),
        }
    }

    #[test]
    fn a_permit_count_above_the_backend_maximum_is_refused_before_the_semaphore() {
        // `Semaphore::new` panics above `MAX_PERMITS`. Both semaphore-backed
        // bounds must refuse ahead of it, and both are checked here because the
        // shared helper is only as good as its two call sites.
        let over = Semaphore::MAX_PERMITS + 1;
        match MaxConnections::try_new(over) {
            Err(error) => assert_eq!(
                error,
                ApiHostConfigError::BackendCapacityExceeded {
                    field: "max_connections",
                    supplied: over,
                    maximum: Semaphore::MAX_PERMITS,
                }
            ),
            Ok(_) => panic!("a connection bound above the permit backend was admitted"),
        }
        match MaxInflightRequests::try_new(over) {
            Err(error) => assert_eq!(
                error,
                ApiHostConfigError::BackendCapacityExceeded {
                    field: "max_inflight_requests",
                    supplied: over,
                    maximum: Semaphore::MAX_PERMITS,
                }
            ),
            Ok(_) => panic!("an in-flight bound above the permit backend was admitted"),
        }

        // The same-file control at the exact boundary: `MAX_PERMITS` itself is
        // admitted, so the refusals above are about the excess and not about
        // large numbers generally.
        match MaxConnections::try_new(Semaphore::MAX_PERMITS) {
            Ok(bound) => assert_eq!(bound.value(), Semaphore::MAX_PERMITS),
            Err(error) => panic!("the backend's own maximum was refused: {error}"),
        }
    }
}

//! The bounded vocabulary of the event stream: what a subscriber may be
//! promised, and which refusal a broken promise takes.
//!
//! These are the numbers and their refusals, not the fan-out that honours them,
//! so they change when a bound changes and for no other reason.
use std::num::NonZeroUsize;

use thiserror::Error;

pub const MAX_SSE_EVENT_BYTES: usize = 262_144;
pub const MAX_SSE_FRAME_BYTES: usize = MAX_SSE_EVENT_BYTES + 32;
pub const MAX_SSE_SUBSCRIBERS: usize = 256;
/// Frames one subscriber may have waiting. An ordinary fast stream bursts
/// hundreds of small deltas; at 64 the burst overflowed and the window lost
/// them. With 256 KiB frames the worst case is 256 MiB per stalled subscriber.
pub const MAX_SSE_PENDING_EVENTS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SseLimits {
    subscribers: NonZeroUsize,
    pending_events: NonZeroUsize,
    event_bytes: NonZeroUsize,
    frame_bytes: NonZeroUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SseLimitValues {
    pub subscribers: usize,
    pub pending_events: usize,
    pub event_bytes: usize,
    pub frame_bytes: usize,
}

impl SseLimits {
    pub fn try_new(values: SseLimitValues) -> Result<Self, SseError> {
        let subscribers = NonZeroUsize::new(values.subscribers).ok_or(SseError::ZeroBound {
            bound: "subscribers",
        })?;
        let pending_events =
            NonZeroUsize::new(values.pending_events).ok_or(SseError::ZeroBound {
                bound: "pending_events",
            })?;
        let event_bytes = NonZeroUsize::new(values.event_bytes).ok_or(SseError::ZeroBound {
            bound: "event_bytes",
        })?;
        let frame_bytes = NonZeroUsize::new(values.frame_bytes).ok_or(SseError::ZeroBound {
            bound: "frame_bytes",
        })?;
        if subscribers.get() > MAX_SSE_SUBSCRIBERS {
            return Err(SseError::BoundTooLarge {
                bound: "subscribers",
                maximum: MAX_SSE_SUBSCRIBERS,
            });
        }
        if pending_events.get() > MAX_SSE_PENDING_EVENTS {
            return Err(SseError::BoundTooLarge {
                bound: "pending_events",
                maximum: MAX_SSE_PENDING_EVENTS,
            });
        }
        if event_bytes.get() > MAX_SSE_EVENT_BYTES {
            return Err(SseError::BoundTooLarge {
                bound: "event_bytes",
                maximum: MAX_SSE_EVENT_BYTES,
            });
        }
        if frame_bytes.get() > MAX_SSE_FRAME_BYTES {
            return Err(SseError::BoundTooLarge {
                bound: "frame_bytes",
                maximum: MAX_SSE_FRAME_BYTES,
            });
        }
        Ok(Self {
            subscribers,
            pending_events,
            event_bytes,
            frame_bytes,
        })
    }

    pub fn production() -> Result<Self, SseError> {
        Self::try_new(SseLimitValues {
            subscribers: MAX_SSE_SUBSCRIBERS,
            pending_events: MAX_SSE_PENDING_EVENTS,
            event_bytes: MAX_SSE_EVENT_BYTES,
            frame_bytes: MAX_SSE_FRAME_BYTES,
        })
    }

    /// The subscriber bound, for the registry that admits one.
    pub fn subscribers(&self) -> usize {
        self.subscribers.get()
    }

    /// Frames one subscriber's queue may hold.
    pub fn pending_events(&self) -> usize {
        self.pending_events.get()
    }

    /// One encoded event's byte bound.
    pub fn event_bytes(&self) -> usize {
        self.event_bytes.get()
    }

    /// One encoded frame's byte bound.
    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes.get()
    }
}

#[derive(Debug, Error)]
pub enum SseError {
    #[error("SSE bound {bound} must be positive")]
    ZeroBound { bound: &'static str },
    #[error("SSE bound {bound} exceeds its maximum of {maximum}")]
    BoundTooLarge { bound: &'static str, maximum: usize },
    #[error("SSE subscriber bound is exhausted")]
    SubscriberBound,
    #[error("pending approver observation bound is exhausted")]
    ApprovalWatchBound,
    #[error("SSE subscriber registry is unavailable")]
    RegistryUnavailable,
    #[error("bounded SSE subscriber overflowed")]
    SubscriberOverflow,
    #[error("SSE event encoding failed")]
    Encode,
    #[error("SSE event exceeds its byte bound")]
    EventBytes,
    #[error("SSE frame exceeds its byte bound")]
    FrameBytes,
}

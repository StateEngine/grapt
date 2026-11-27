// Copyright 2023-2026 SpaceXAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: explicit stream lifetimes, finite outage budget, no clock or SDK.

use std::fmt;
use std::time::Duration;

const STABLE_STREAM_THRESHOLD: Duration = Duration::from_secs(2);
const BASE_DELAY: Duration = Duration::from_millis(500);
const MAX_DELAY: Duration = Duration::from_secs(30);
const MAX_SERVER_DELAY: Duration = Duration::from_secs(24 * 3600);
const MAX_RAPID_RECONNECTS: u32 = 16;

/// Reconnection is exhausted for this outage; never replay a POST to recover it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectExhausted;

impl fmt::Display for ReconnectExhausted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MCP SSE reconnection budget exhausted")
    }
}

impl std::error::Error for ReconnectExhausted {}

/// One stream's finite recovery budget. Healthy streams start a new episode.
///
/// The caller supplies the observed lifetime; this policy reads no clock and
/// sleeps nowhere. A failed connection counts as a zero-lifetime stream.
#[derive(Debug, Default)]
pub struct ReconnectPolicy {
    rapid_reconnects: u32,
}

impl ReconnectPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_delay(
        &mut self,
        stream_lifetime: Duration,
        server_retry: Option<Duration>,
    ) -> Result<Duration, ReconnectExhausted> {
        // A hint is a minimum wait. Refuse an excessive hint instead of
        // reconnecting earlier than the server requested (MCP 2025-11-25).
        if server_retry.is_some_and(|delay| delay > MAX_SERVER_DELAY) {
            return Err(ReconnectExhausted);
        }
        if stream_lifetime >= STABLE_STREAM_THRESHOLD {
            self.rapid_reconnects = 0;
        }
        if self.rapid_reconnects >= MAX_RAPID_RECONNECTS {
            return Err(ReconnectExhausted);
        }
        self.rapid_reconnects += 1;
        let delay = if self.rapid_reconnects < 2 {
            Duration::ZERO
        } else {
            let exp = self.rapid_reconnects.saturating_sub(2).min(6);
            (BASE_DELAY * 2u32.pow(exp)).min(MAX_DELAY)
        };
        Ok(delay.max(server_retry.unwrap_or_default()))
    }
}

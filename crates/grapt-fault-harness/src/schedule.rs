//! Fault plans, deterministic schedules, and reproducible failure transcripts.

use std::fmt;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::seed::{FaultSeed, SeededRng};

/// When a scripted transport fault rule matches an incoming request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaultTrigger {
    /// Fire on one exact 1-based request number.
    OnRequest(u64),
    /// Fire on every `n`-th request (`request_index % n == 0`, when `n > 0`).
    EveryNthRequest(u64),
    /// Fire whenever the request path starts with `prefix`.
    OnPathPrefix(String),
    /// Fire probabilistically using the schedule's seeded PRNG (`bps` out of 10,000).
    SeededChanceBps(u16),
    /// Always match (typically used for upstream-down simulation).
    Always,
}

/// Fault action injected by [`crate::ChaosProxy`] when a rule triggers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaultAction {
    /// Forward traffic unmodified.
    PassThrough,
    /// Forward `after_bytes` of the upstream response body, then close with EOF.
    MidStreamEofBytes { after_bytes: usize },
    /// Forward `after_frames` complete SSE frames (`\n\n` delimited), then close with EOF.
    MidStreamEofFrames { after_frames: usize },
    /// Forward `after_bytes` of the upstream response, then abruptly drop the connection.
    MidStreamRst { after_bytes: usize },
    /// Split the upstream response at `after_bytes`, perform deterministic yield steps
    /// (and optionally wait for an explicit release gate), then flush the remainder.
    DelayChunk {
        after_bytes: usize,
        yield_steps: u32,
        wait_for_gate: bool,
    },
    /// Return a synthetic HTTP error status (e.g. 429, 503, 529) with optional `Retry-After`.
    InjectHttpStatus {
        status: u16,
        retry_after_secs: Option<u64>,
        body: String,
    },
    /// Truncate the upstream response body to `keep_bytes` (capable of splitting UTF-8 or SSE
    /// frames mid-sequence) and close the socket.
    TruncateBody { keep_bytes: usize },
    /// Simulate an unreachable or degraded upstream service without contacting upstream.
    UpstreamDown { status: u16, reason: String },
}

/// One rule in a [`FaultPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultRule {
    pub trigger: FaultTrigger,
    pub action: FaultAction,
}

/// Declarative fault plan paired with a [`FaultSeed`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultPlan {
    pub name: String,
    pub seed: FaultSeed,
    pub rules: Vec<FaultRule>,
}

impl FaultPlan {
    /// Create an empty plan for `name` and `seed`.
    #[must_use]
    pub fn new(name: impl Into<String>, seed: FaultSeed) -> Self {
        Self {
            name: name.into(),
            seed,
            rules: Vec::new(),
        }
    }

    /// Append a deterministic rule to the plan.
    #[must_use]
    pub fn with_rule(mut self, trigger: FaultTrigger, action: FaultAction) -> Self {
        self.rules.push(FaultRule { trigger, action });
        self
    }

    /// Build a seeded chaos plan that picks deterministic transport faults from a catalog.
    #[must_use]
    pub fn seeded_http_chaos(name: impl Into<String>, seed: FaultSeed, fault_count: u64) -> Self {
        let mut rng = seed.derive("http-chaos").rng();
        let mut plan = Self::new(name, seed);
        for req_index in 1..=fault_count {
            let action = match rng.gen_range_u64(4) {
                0 => FaultAction::InjectHttpStatus {
                    status: 429,
                    retry_after_secs: Some(1 + rng.gen_range_u64(5)),
                    body: "rate limited".to_owned(),
                },
                1 => FaultAction::InjectHttpStatus {
                    status: 503,
                    retry_after_secs: Some(1 + rng.gen_range_u64(3)),
                    body: "service unavailable".to_owned(),
                },
                2 => FaultAction::InjectHttpStatus {
                    status: 529,
                    retry_after_secs: Some(2 + rng.gen_range_u64(4)),
                    body: "overloaded".to_owned(),
                },
                _ => FaultAction::DelayChunk {
                    after_bytes: 16 + rng.gen_range_usize(48),
                    yield_steps: 2,
                    wait_for_gate: false,
                },
            };
            plan = plan.with_rule(FaultTrigger::OnRequest(req_index), action);
        }
        plan
    }
}

/// One ordered event recorded during a chaos run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultEvent {
    pub step: u64,
    pub subsystem: String,
    pub subject: String,
    pub action: String,
}

/// Reproducible transcript emitted by a [`FaultSchedule`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureTranscript {
    pub plan_name: String,
    pub seed: FaultSeed,
    pub failure_reason: Option<String>,
    pub events: Vec<FaultEvent>,
}

impl FailureTranscript {
    /// Format a single-line replay banner containing the seed and failure summary.
    #[must_use]
    pub fn replay_banner(&self) -> String {
        let reason = self.failure_reason.as_deref().unwrap_or("completed");
        format!(
            "chaos-replay plan={} seed={} events={} status={}",
            self.plan_name,
            self.seed.as_u64(),
            self.events.len(),
            reason
        )
    }

    /// Serialize this transcript as deterministic pretty JSON.
    ///
    /// # Errors
    /// Returns a JSON serialization error only if serialization fails.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

impl fmt::Display for FailureTranscript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.replay_banner())?;
        for event in &self.events {
            writeln!(
                f,
                "  #{}: [{}] {} -> {}",
                event.step, event.subsystem, event.subject, event.action
            )?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ScheduleState {
    plan: FaultPlan,
    rng: SeededRng,
    request_counter: u64,
    step_counter: u64,
    events: Vec<FaultEvent>,
    failure_reason: Option<String>,
}

/// Thread-safe deterministic fault-schedule controller shared across a chaos run.
#[derive(Debug, Clone)]
pub struct FaultSchedule {
    inner: Arc<Mutex<ScheduleState>>,
}

impl FaultSchedule {
    /// Construct a schedule controller from a [`FaultPlan`].
    #[must_use]
    pub fn new(plan: FaultPlan) -> Self {
        let rng = plan.seed.rng();
        Self {
            inner: Arc::new(Mutex::new(ScheduleState {
                plan,
                rng,
                request_counter: 0,
                step_counter: 0,
                events: Vec::new(),
                failure_reason: None,
            })),
        }
    }

    /// Return the seed governing this schedule.
    #[must_use]
    pub fn seed(&self) -> FaultSeed {
        let guard = self.lock_state();
        guard.plan.seed
    }

    /// Evaluate the next incoming HTTP/SSE request against the plan and record the decision.
    #[must_use]
    pub fn next_request_action(&self, method: &str, path: &str) -> FaultAction {
        let mut guard = self.lock_state();
        guard.request_counter = guard.request_counter.saturating_add(1);
        let req_index = guard.request_counter;
        let rules = guard.plan.rules.clone();

        let mut chosen = FaultAction::PassThrough;
        for rule in &rules {
            let matched = match &rule.trigger {
                FaultTrigger::OnRequest(target) => req_index == *target,
                FaultTrigger::EveryNthRequest(n) => *n > 0 && req_index.is_multiple_of(*n),
                FaultTrigger::OnPathPrefix(prefix) => path.starts_with(prefix.as_str()),
                FaultTrigger::SeededChanceBps(bps) => guard.rng.chance_bps(*bps),
                FaultTrigger::Always => true,
            };
            if matched {
                chosen = rule.action.clone();
                break;
            }
        }

        guard.step_counter = guard.step_counter.saturating_add(1);
        let step = guard.step_counter;
        guard.events.push(FaultEvent {
            step,
            subsystem: "proxy".to_owned(),
            subject: format!("req#{req_index} {method} {path}"),
            action: format!("{chosen:?}"),
        });
        chosen
    }

    /// Record a custom event (e.g., from storage hooks or a client reconnect).
    pub fn record_event(
        &self,
        subsystem: impl Into<String>,
        subject: impl Into<String>,
        action: impl Into<String>,
    ) {
        let mut guard = self.lock_state();
        guard.step_counter = guard.step_counter.saturating_add(1);
        let step = guard.step_counter;
        guard.events.push(FaultEvent {
            step,
            subsystem: subsystem.into(),
            subject: subject.into(),
            action: action.into(),
        });
    }

    /// Mark the run as failed and return the reproducible [`FailureTranscript`].
    #[must_use]
    pub fn record_failure(&self, reason: impl Into<String>) -> FailureTranscript {
        let mut guard = self.lock_state();
        guard.failure_reason = Some(reason.into());
        FailureTranscript {
            plan_name: guard.plan.name.clone(),
            seed: guard.plan.seed,
            failure_reason: guard.failure_reason.clone(),
            events: guard.events.clone(),
        }
    }

    /// Snapshot the current [`FailureTranscript`].
    #[must_use]
    pub fn transcript(&self) -> FailureTranscript {
        let guard = self.lock_state();
        FailureTranscript {
            plan_name: guard.plan.name.clone(),
            seed: guard.plan.seed,
            failure_reason: guard.failure_reason.clone(),
            events: guard.events.clone(),
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ScheduleState> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

//! Cache observations in the existing transcript projection, independent of cost.

mod observed_calls;
pub use observed_calls::{CallCost, CallObservation, CostEstimate, ObservedCalls};
mod request_change;
pub use request_change::RequestChange;

// Estimates reaching these status lines have already been validated. Keep a
// positive amount visible even when it falls below the six-decimal precision.
fn usd_amount(amount: f64) -> String {
    if amount > 0.0 && amount < 0.000001 {
        "<0.000001".into()
    } else {
        format!("{amount:.6}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CacheUsage {
    /// Product estimates this exact assistant attempt using its pinned API policy.
    pub estimated_usd: Option<f64>,
    pub input: Option<u64>,
    /// Last provider context coordinate: total, input plus output, or input lower bound.
    pub context: Option<u64>,
    pub read: Option<u64>,
    pub request_change: Option<RequestChange>,
    pub observed_calls: Option<CallObservation>,
    pub usage_warning: Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub struct CacheSummary {
    observed_calls: Option<ObservedCalls>,
    capture_changed: bool,
    calls_unreadable: bool,
    pub latest: CacheUsage,
    last_miss: Option<CacheUsage>,
    input: u64,
    read: u64,
    measured: usize,
    unreported: usize,
    overflowed: bool,
    history_complete: bool,
    cost_usd: f64,
    cost_measured: usize,
    cost_unreported: usize,
    cost_overflow: bool,
}

impl CacheSummary {
    pub fn history_complete(&mut self, complete: bool) {
        self.history_complete = complete;
    }

    /// History recovery builds a new projection and folds each assistant row once.
    pub fn observe(&mut self, usage: CacheUsage) {
        match usage.observed_calls {
            Some(CallObservation::Unreadable) => self.calls_unreadable = true,
            Some(CallObservation::Reported(newer)) => self.merge_calls(newer),
            None => {}
        }
        match usage
            .estimated_usd
            .filter(|amount| amount.is_finite() && *amount >= 0.0)
        {
            Some(amount) => {
                self.cost_measured = self.cost_measured.saturating_add(1);
                let sum = self.cost_usd + amount;
                if sum.is_finite() {
                    self.cost_usd = sum;
                } else {
                    self.cost_overflow = true;
                }
            }
            None => self.cost_unreported = self.cost_unreported.saturating_add(1),
        }
        self.latest = usage;
        match (usage.input, usage.read) {
            (Some(input), Some(read)) if read <= input => {
                if read < input {
                    self.last_miss = Some(usage);
                }
                self.measured = self.measured.saturating_add(1);
                if let (Some(input), Some(read)) =
                    (self.input.checked_add(input), self.read.checked_add(read))
                {
                    self.input = input;
                    self.read = read;
                } else {
                    self.overflowed = true;
                }
            }
            _ => self.unreported = self.unreported.saturating_add(1),
        }
    }

    fn merge_calls(&mut self, newer: ObservedCalls) {
        match self.observed_calls {
            Some(older) if older.scope != newer.scope => self.capture_changed = true,
            Some(older) => {
                let latest = if older.attempts > newer.attempts {
                    older
                } else {
                    newer
                };
                // Refreshed rows and live updates can carry older snapshots.
                // Missing evidence never recovers within the same capture.
                self.observed_calls = Some(ObservedCalls {
                    cost: latest.cost.preserve_gaps(older.cost, newer.cost),
                    unreported: older.unreported.max(newer.unreported),
                    loss: older.loss.max(newer.loss),
                    overflow: older.overflow || newer.overflow,
                    write: match (older.write, newer.write) {
                        (Some(_), Some(_)) => latest.write,
                        _ => None,
                    },
                    ..latest
                });
            }
            None => self.observed_calls = Some(newer),
        }
    }

    /// Fold an OLDER window's observations in before this one's, once.
    ///
    /// Chronological: what this summary already holds is newer, so `latest`
    /// and `last_miss` keep it — even an unreported newer observation is the
    /// newest word — and the older window fills them only where this one has
    /// nothing. Counters add; unknown, loss and overflow carry. Coverage is
    /// NOT decided here: `history_complete` stays whatever the owner set.
    pub fn prepend(&mut self, older: Self) {
        let newer = std::mem::replace(self, older);
        let complete = newer.history_complete;
        if let Some(calls) = newer.observed_calls {
            self.merge_calls(calls);
        }
        self.capture_changed |= newer.capture_changed;
        self.calls_unreadable |= newer.calls_unreadable;
        if newer.measured.saturating_add(newer.unreported) > 0 {
            self.latest = newer.latest;
        }
        self.last_miss = newer.last_miss.or(self.last_miss);
        match (
            self.input.checked_add(newer.input),
            self.read.checked_add(newer.read),
        ) {
            (Some(input), Some(read)) => {
                self.input = input;
                self.read = read;
            }
            _ => self.overflowed = true,
        }
        self.overflowed |= newer.overflowed;
        self.measured = self.measured.saturating_add(newer.measured);
        self.unreported = self.unreported.saturating_add(newer.unreported);
        self.cost_usd += newer.cost_usd;
        self.cost_overflow |= newer.cost_overflow || !self.cost_usd.is_finite();
        self.cost_measured = self.cost_measured.saturating_add(newer.cost_measured);
        self.cost_unreported = self.cost_unreported.saturating_add(newer.cost_unreported);
        self.history_complete = complete;
    }

    pub fn cost_status(&self) -> String {
        if self.cost_overflow {
            return "unavailable: estimate overflow".into();
        }
        if self.cost_measured == 0 {
            return "unavailable: pricing or usage unreported".into();
        }
        let coverage = if self.history_complete && self.cost_unreported == 0 {
            "assistant history".to_owned()
        } else {
            format!(
                "subtotal; {} unpriced; history {}",
                crate::format::count(self.cost_unreported as u64),
                if self.history_complete {
                    "complete"
                } else {
                    "incomplete"
                }
            )
        };
        format!(
            "estimated API USD {} ({coverage}; retries/compaction excluded)",
            usd_amount(self.cost_usd)
        )
    }

    /// Tool hints may reread older draft rows. Full reconciliation replaces
    /// the projection; advisory refreshes cannot remove settled observations.
    pub fn refresh(&mut self, newer: Self) {
        if newer.measured.saturating_add(newer.unreported)
            >= self.measured.saturating_add(self.unreported)
        {
            *self = newer;
        }
    }

    pub fn latest_share(&self) -> Option<f64> {
        share(self.latest.input?, self.latest.read?)
    }

    /// Weighted assistant-history share; separate provider compaction calls are not in this DTO.
    pub fn history_share(&self) -> Option<f64> {
        if !self.history_complete || self.unreported != 0 || self.overflowed {
            return None;
        }
        share(self.input, self.read)
    }

    pub fn calls_status(&self) -> Option<String> {
        if self.calls_unreadable {
            return Some("calls - · accounting data unreadable".to_owned());
        }
        if self.capture_changed {
            return Some("calls - · capture changed during history refresh".to_owned());
        }
        self.observed_calls.map(ObservedCalls::status)
    }

    pub fn miss_status(&self) -> Option<String> {
        let miss = self.last_miss?;
        let request = miss
            .request_change
            .map(|change| format!(" · request: {}", change.description()))
            .unwrap_or_default();
        // A request difference is evidence, not a provider-reported cause.
        Some(format!("last observed miss: cause unreported{request}"))
    }

    pub fn status(&self) -> String {
        let percent = |value: Option<f64>| {
            value
                .map(|value| format!("{:.1}%", value * 100.0))
                .unwrap_or_else(|| "-".to_owned())
        };
        let note = if self.overflowed {
            " · accounting overflow".to_owned()
        } else if !self.history_complete {
            " · history incomplete".to_owned()
        } else if self.unreported != 0 {
            let noun = if self.unreported == 1 {
                "message"
            } else {
                "messages"
            };
            format!(
                " · {} {noun} unreported",
                crate::format::count(self.unreported as u64)
            )
        } else if self.latest.input == Some(0) && self.latest.read == Some(0) {
            " · no input".to_owned()
        } else if self
            .latest
            .input
            .is_some_and(|input| self.latest.read.is_some_and(|read| read < input))
        {
            // New input can accompany a cache hit; usage alone cannot explain
            // why tokens were not read from cache.
            " · cause unreported".to_owned()
        } else if self.measured == 0 {
            " · usage unreported".to_owned()
        } else {
            String::new()
        };
        let request = self
            .latest
            .request_change
            .map(|change| format!(" · request: {}", change.description()))
            .unwrap_or_default();
        let warning = self
            .latest
            .usage_warning
            .map(|warning| {
                format!(
                    "warning: provider usage {} is {}; answer preserved · ",
                    warning.field_word(),
                    warning.issue.word()
                )
            })
            .unwrap_or_default();
        format!(
            "{warning}cache read {} · history {}{note}{request}",
            percent(self.latest_share()),
            percent(self.history_share())
        )
    }
}

fn share(input: u64, read: u64) -> Option<f64> {
    (input != 0 && read <= input).then(|| read as f64 / input as f64)
}

#[cfg(test)]
mod tests;

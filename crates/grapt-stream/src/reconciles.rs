//! Process-local diagnostics for global recovery boundaries, not subscriber overflow.
use serde::Serialize;
use std::sync::Mutex;

/// The fixed causes that require clients to recover from durable truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileReason {
    UnpublishedAdvisoryNotice,
    UnrepresentableFrame,
    ProjectionFailure,
}

impl ReconcileReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnpublishedAdvisoryNotice => "unpublished_advisory_notice",
            Self::UnrepresentableFrame => "unrepresentable_frame",
            Self::ProjectionFailure => "projection_failure",
        }
    }
}

/// Lifetime counts for this hub; restart resets them and saturation never wraps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ReconcileCounts {
    pub unpublished_advisory_notice: u64,
    pub unrepresentable_frame: u64,
    pub projection_failure: u64,
}

#[derive(Debug, Default)]
pub struct ReconcileCounters(Mutex<ReconcileCounts>);

impl ReconcileCounters {
    pub fn record(&self, reason: ReconcileReason) {
        let mut counts = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = match reason {
            ReconcileReason::UnpublishedAdvisoryNotice => &mut counts.unpublished_advisory_notice,
            ReconcileReason::UnrepresentableFrame => &mut counts.unrepresentable_frame,
            ReconcileReason::ProjectionFailure => &mut counts.projection_failure,
        };
        *count = count.saturating_add(1);
    }

    pub fn snapshot(&self) -> ReconcileCounts {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_counters_saturate_without_wrapping_or_changing_another_reason() {
        let counts = ReconcileCounters(Mutex::new(ReconcileCounts {
            unrepresentable_frame: u64::MAX,
            ..ReconcileCounts::default()
        }));
        counts.record(ReconcileReason::UnrepresentableFrame);
        assert_eq!(
            counts.snapshot(),
            ReconcileCounts {
                unrepresentable_frame: u64::MAX,
                ..ReconcileCounts::default()
            }
        );
    }
}

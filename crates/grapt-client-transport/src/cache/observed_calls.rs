//! The capture's all-dispatch totals are separate from assistant-only history.
mod cost;
pub use cost::{CallCost, CostEstimate};
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CallObservation {
    Reported(ObservedCalls),
    Unreadable,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObservedCalls {
    pub(crate) cost: CallCost,
    pub(crate) scope: [u8; 64],
    pub(crate) attempts: u64,
    pub(crate) unreported: u64,
    pub(crate) input: u64,
    pub(crate) read: u64,
    pub(crate) write: Option<u64>,
    pub(crate) loss: u64,
    pub(crate) overflow: bool,
}
impl ObservedCalls {
    pub(super) fn status(self) -> String {
        let complete = self.unreported == 0 && self.loss == 0 && !self.overflow;
        let share = complete
            .then(|| super::share(self.input, self.read))
            .flatten()
            .map(|share| format!("{:.1}%", share * 100.0))
            .unwrap_or_else(|| "-".to_owned());
        let uncached = complete
            .then(|| self.input.checked_sub(self.read))
            .flatten()
            .map(crate::format::count)
            .unwrap_or_else(|| "-".to_owned());
        let write = (!self.overflow && self.loss == 0)
            .then_some(self.write)
            .flatten()
            .map(crate::format::count)
            .unwrap_or_else(|| "-".to_owned());
        let note = if self.overflow {
            " · accounting overflow".to_owned()
        } else if self.loss != 0 {
            " · capture incomplete".to_owned()
        } else if self.unreported != 0 {
            format!(" · {} unreported", crate::format::count(self.unreported))
        } else {
            String::new()
        };
        let cost = self
            .cost
            .status(self.attempts, self.loss != 0 || self.overflow);
        format!(
            "calls {share} · uncached {uncached} · write {write} · {cost} ({} observed){note}",
            crate::format::count(self.attempts)
        )
    }
}

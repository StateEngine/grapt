use crate::{EventStore, GovernedTransaction};
use grapt_event::AggregateId;
use grapt_types::timing::{
    TimingClass, TimingContext, TimingObserver, TimingScope, TimingSpan, TimingStage,
};
use std::sync::Arc;
impl EventStore {
    /// Installs a volatile diagnostic sink; no schema or write policy changes.
    pub fn install_timing_observer(&mut self, observer: Arc<dyn TimingObserver>) {
        self.timing = Some(observer);
    }
}
impl GovernedTransaction<'_> {
    pub(crate) fn attribute_timing(&self, aggregate: &AggregateId, name: &str) {
        let class = match name {
            "session.content.appended" => TimingClass::Content,
            "session.usage.observed" => TimingClass::Usage,
            "session.step.ended" | "session.step.failed" => TimingClass::Step,
            "session.execution.succeeded"
            | "session.execution.failed"
            | "session.execution.interrupted" => TimingClass::Execution,
            _ => TimingClass::Other,
        };
        let mut scope = self.timing_scope.borrow_mut();
        // A terminal and its receipt remain one commit. Other facts appended
        // to that transaction cannot relabel its tail owner.
        if scope.class == TimingClass::Other {
            *scope = TimingScope {
                session: match aggregate {
                    AggregateId::Session(id) => Some(id.clone()),
                    _ => None,
                },
                attempt: None,
                class,
            };
        }
    }
    pub fn timing_span(&self, stage: TimingStage) -> Option<TimingSpan> {
        self.timing.as_ref().map(|observer| {
            TimingContext {
                observer: Arc::clone(observer),
                scope: self.timing_scope.borrow().clone(),
            }
            .span(stage)
        })
    }
}

#[cfg(test)]
mod tests;

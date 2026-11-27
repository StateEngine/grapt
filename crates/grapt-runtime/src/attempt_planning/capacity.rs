// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified for wait-status edges.
//! Advisory edges around a real, cancellable transport-capacity wait.
use std::sync::Arc;

struct Waiting {
    observer: Option<Arc<dyn Fn(Option<usize>) + Send + Sync>>,
    position: Option<usize>,
}
impl Waiting {
    fn observe(&mut self, position: usize) {
        if self.position != Some(position) {
            self.position = Some(position);
            if let Some(observer) = &self.observer {
                observer(Some(position));
            }
        }
    }
}
impl Drop for Waiting {
    fn drop(&mut self) {
        if self.position.is_some() {
            if let Some(observer) = &self.observer {
                observer(None);
            }
        }
    }
}

pub(crate) async fn reserve(
    opener: &grapt_provider::AttemptOpener,
    observer: Option<Arc<dyn Fn(Option<usize>) + Send + Sync>>,
    progress: &tokio::sync::watch::Sender<()>,
    child_capacity: Option<std::num::NonZeroUsize>,
) -> Result<grapt_provider::AttemptCapacity, grapt_provider::CodexError> {
    let mut notice = Waiting {
        observer,
        position: None,
    };
    let observer = |position, progressed| {
        notice.observe(position);
        if progressed {
            progress.send_replace(());
        }
    };
    match child_capacity {
        Some(limit) => {
            opener
                .reserve_child_capacity_observed(limit, observer)
                .await
        }
        None => opener.reserve_capacity_observed(observer).await,
    }
}

pub(crate) fn refusal(error: grapt_provider::CodexError) -> crate::AttemptAdmissionRefusal {
    let detail = error.to_string();
    match error {
        grapt_provider::CodexError::ActiveAttemptCapacity { .. }
        | grapt_provider::CodexError::AttemptWaiterCapacity { .. } => {
            crate::AttemptAdmissionRefusal::Saturated { detail }
        }
        _ => crate::AttemptAdmissionRefusal::OpenRefused { detail },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_contention_is_retryable_and_a_registry_fault_stays_a_fault() {
        for error in [
            grapt_provider::CodexError::ActiveAttemptCapacity { capacity: 1 },
            grapt_provider::CodexError::AttemptWaiterCapacity { max_waiters: 1024 },
        ] {
            let refusal = refusal(error);
            assert!(matches!(
                refusal,
                crate::AttemptAdmissionRefusal::Saturated { .. }
            ));
            let message = refusal.to_string();
            assert!(
                message.contains("retryable") && message.contains("followup_task"),
                "{message}"
            );
        }
        assert!(matches!(
            refusal(grapt_provider::CodexError::RegistryPoisoned),
            crate::AttemptAdmissionRefusal::OpenRefused { .. }
        ));
    }
}

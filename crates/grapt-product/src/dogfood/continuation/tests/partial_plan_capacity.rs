//! Reader contention after target selection must not spend another target.
use super::*;
use crate::dogfood::production::{ProductPlan, ProductTargetAuthority};
use grapt_runtime::provider_source::PlanRefusalCause;
use grapt_runtime::{AttemptPlan, HistoryGeneration};
use grapt_store::read_pool::{ReadLease, ReadPool, READ_CAPACITY};
use grapt_types::ResolvedModelTarget;
use std::sync::atomic::AtomicBool;

struct SaturateAfterSelection {
    inner: Arc<dyn ProductTargetAuthority>,
    pool: ReadPool,
    held: Mutex<Vec<ReadLease>>,
    armed: AtomicBool,
    issued: AtomicUsize,
}

impl ProductTargetAuthority for SaturateAfterSelection {
    fn protocol_family(&self) -> grapt_provider::ProtocolFamily {
        self.inner.protocol_family()
    }

    fn issue(&self) -> Result<ResolvedModelTarget, String> {
        self.issued.fetch_add(1, Ordering::SeqCst);
        self.inner.issue()
    }

    fn context_tokens(&self) -> Option<u64> {
        self.inner.context_tokens()
    }

    fn effort_policy(&self) -> grapt_provider::EffortPolicy {
        if self.armed.swap(false, Ordering::SeqCst) {
            let mut held = match self.held.lock() {
                Ok(held) => held,
                Err(error) => panic!("reader-holder lock: {error}"),
            };
            for _ in 0..READ_CAPACITY {
                held.push(match self.pool.checkout() {
                    Ok(lease) => lease,
                    Err(error) => panic!("reader saturation: {error}"),
                });
            }
        }
        self.inner.effort_policy()
    }
}

#[test]
fn partial_plan_reader_contention_recovers_without_reissuing_its_initial_target(
) -> Result<(), String> {
    let fixture = Fixture::new("partial-plan-reader-contention")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let pool = lock(fixture.inbox()?)
        .store()
        .read_pool()
        .map_err(|error| error.to_string())?
        .ok_or("file reader pool absent")?;
    let family = grapt_provider::ProtocolFamily::ChatGptCodex;
    let targets = Arc::new(SaturateAfterSelection {
        inner: crate::dogfood::production::tests::projection_targets(family)?,
        pool,
        held: Mutex::new(Vec::new()),
        armed: AtomicBool::new(true),
        issued: AtomicUsize::new(0),
    });
    let first = targets.issue()?;
    let step = StepNumber::new(1).ok_or("step")?;
    let generation = HistoryGeneration::new(1);
    let mut plan = ProductPlan::new(
        family,
        "inspect".into(),
        grapt_provider::PromptCacheKey::for_session(&fixture.session)
            .map_err(|error| error.to_string())?,
        Some(first),
        targets.clone(),
        Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        fixture.continuation(fixture.input.clone())?,
        step,
    );
    let refused = plan
        .plan(step, 1, generation)
        .err()
        .ok_or("saturated planning produced a request")?;
    assert_eq!(refused.cause, PlanRefusalCause::CapacityBusy);
    assert_eq!(targets.issued.load(Ordering::SeqCst), 1);
    let held = std::mem::take(&mut *targets.held.lock().map_err(|error| error.to_string())?);
    assert_eq!(held.len(), READ_CAPACITY);
    for lease in held {
        lease.finish().map_err(|error| error.to_string())?;
    }
    plan.plan(step, 1, generation)
        .map_err(|refusal| format!("released planning did not recover: {}", refusal.detail))?;
    assert_eq!(
        targets.issued.load(Ordering::SeqCst),
        1,
        "local-capacity recovery spent a second target for the same attempt"
    );
    Ok(())
}

use grapt_runtime::mailbox::remote::credential::{
    CredentialBudget, CredentialEvent as Event, Disposition, ProvisionGeneration, RefusalCause,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn stale_outcomes_cannot_change_a_newer_permanent_refusal() -> TestResult {
    let old = ProvisionGeneration::FIRST;
    let current = old.next().ok_or("generation")?;
    for (event, cause) in [
        (Event::ProjectMismatch, RefusalCause::Principal),
        (Event::Unauthenticated, RefusalCause::Capability),
    ] {
        let mut budget = CredentialBudget::default();
        budget.provisioned(old);
        budget.provisioned(current);
        assert_eq!(
            budget.observe(Some(current), event),
            Disposition::Park(cause)
        );
        let refused = budget.clone();
        for late in [Event::FloorChanged, Event::Live, Event::Transport] {
            assert_eq!(budget.observe(Some(old), late), Disposition::Ignore);
            assert_eq!(budget, refused, "stale outcome changed current custody");
        }
        assert_eq!(
            budget.observe(Some(current), Event::FloorChanged),
            Disposition::Park(cause)
        );
        assert_eq!(
            budget.observe(Some(current), Event::Live),
            Disposition::Park(cause)
        );
        assert_eq!(budget.refusal(), Some(cause));
    }
    Ok(())
}

#[test]
fn stale_live_and_transport_failures_do_not_renew_the_floor_allowance() -> TestResult {
    let old = ProvisionGeneration::FIRST;
    let current = old.next().ok_or("generation")?;
    let future = current.next().ok_or("generation")?;
    let mut budget = CredentialBudget::default();
    budget.provisioned(current);
    for _ in 0..2 {
        assert_eq!(
            budget.observe(Some(current), Event::FloorChanged),
            Disposition::Retry
        );
    }
    let held = budget.clone();
    for stamp in [Some(old), Some(future), None] {
        assert_eq!(budget.observe(stamp, Event::Live), Disposition::Ignore);
        assert_eq!(budget, held);
    }
    budget.provisioned(old);
    budget.provisioned(current);
    assert_eq!(
        budget, held,
        "an old or repeated provision renewed the allowance"
    );
    assert_eq!(
        budget.observe(Some(current), Event::Transport),
        Disposition::Retry
    );
    assert_eq!(
        budget.observe(Some(current), Event::FloorChanged),
        Disposition::Retry
    );
    assert_eq!(
        budget.observe(Some(current), Event::Transport),
        Disposition::Retry
    );
    assert_eq!(
        budget.observe(Some(current), Event::FloorChanged),
        Disposition::Park(RefusalCause::Floor)
    );
    Ok(())
}

#[test]
fn a_newer_provision_recovers_absence_and_may_skip_unobserved_provisions() -> TestResult {
    let first = ProvisionGeneration::FIRST;
    let newest = first
        .next()
        .and_then(ProvisionGeneration::next)
        .ok_or("generation")?;
    let mut budget = CredentialBudget::default();
    assert_eq!(
        budget.observe(None, Event::Unauthenticated),
        Disposition::Ignore
    );
    budget.absent();
    assert_eq!(budget.refusal(), Some(RefusalCause::Capability));
    budget.provisioned(first);
    assert_eq!(budget.refusal(), None);
    assert_eq!(
        budget.observe(Some(first), Event::Unauthenticated),
        Disposition::Park(RefusalCause::Capability)
    );
    budget.provisioned(newest);
    assert_eq!(newest.get(), 3);
    assert_eq!(budget.refusal(), None);
    for _ in 0..3 {
        assert_eq!(
            budget.observe(Some(newest), Event::FloorChanged),
            Disposition::Retry
        );
    }
    assert_eq!(
        budget.observe(Some(newest), Event::FloorChanged),
        Disposition::Park(RefusalCause::Floor)
    );
    Ok(())
}

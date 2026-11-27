// ---------------------------------------------------------------------------
// Successful switch
// ---------------------------------------------------------------------------

#[test]
fn a_switch_into_an_empty_slot_commits_at_the_first_generation() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;

    let candidate = attributed(owner("inst-a", 0, 4242, 8080).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let discovered = controller.discover()?;
    let prepared = controller.prepare(&discovered, &candidate)?;
    let receipt = controller.commit(prepared, Retirement::NoIncumbent)?;

    assert_eq!(
        receipt.outcome(),
        ReplacementOutcome::Replaced,
        "claiming an empty slot with a retired-nothing incumbent is a completed replacement"
    );
    let committed = receipt
        .committed_owner()
        .ok_or("a committed switch names its owner")?;
    assert_eq!(
        committed.generation(),
        ServiceGeneration::FIRST,
        "the first owner of an empty slot holds the first generation"
    );
    let now = registered(&controller)?.ok_or("the slot must now name an owner")?;
    assert!(
        now.is_same_owner(committed),
        "the registration read back after the commit must name the committed owner"
    );
    Ok(())
}

#[test]
fn succeeding_an_incumbent_advances_the_generation_by_exactly_one() -> R {
    let incumbent = owner("inst-a", 3, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;

    // Generation 4 is the successor of 3; the candidate must declare exactly it.
    let candidate = attributed(owner("inst-b", 4, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let discovered = controller.discover()?;
    let prepared = controller.prepare(&discovered, &candidate)?;
    let receipt = controller.commit(prepared, Retirement::Retired)?;

    assert_eq!(
        receipt.outcome(),
        ReplacementOutcome::Replaced,
        "a committed switch whose incumbent retired is a completed replacement"
    );
    let committed = receipt
        .committed_owner()
        .ok_or("a committed switch names its owner")?;
    assert_eq!(
        committed.generation().value(),
        4,
        "ownership transfer advances the generation by exactly one and never rewinds"
    );
    Ok(())
}

#[test]
fn the_registrations_instant_comes_from_the_injected_clock() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(1_700_000_000_123);
    let controller = controller(&store, &clock)?;

    let candidate = attributed(owner("inst-a", 0, 4242, 8080).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let discovered = controller.discover()?;
    let prepared = controller.prepare(&discovered, &candidate)?;
    controller.commit(prepared, Retirement::NoIncumbent)?;

    let bytes = store
        .read_registration()?
        .ok_or("the slot must hold a registration")?;
    let written = RegistrationRecord::decode(&bytes)?;
    assert_eq!(
        written.registered_at().as_millis(),
        1_700_000_000_123,
        "the registration's instant must be the injected clock's, not an ambient or constant one"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Retire warning — master law 4
// ---------------------------------------------------------------------------

#[test]
fn a_retirement_that_timed_out_is_a_success_with_warning_not_a_rollback() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;

    let candidate = attributed(owner("inst-b", 2, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let discovered = controller.discover()?;
    let prepared = controller.prepare(&discovered, &candidate)?;
    let receipt = controller.commit(prepared, Retirement::TimedOut)?;

    assert_eq!(
        receipt.outcome(),
        ReplacementOutcome::ReplacedRetirePending,
        "a post-commit retirement that timed out is a replacement with a pending retire, and \
         reporting it as RolledBack would deny that the candidate is already current"
    );
    let committed = receipt
        .committed_owner()
        .ok_or("a retire-pending switch still committed")?;
    let now = registered(&controller)?.ok_or("the slot must still name an owner")?;
    assert!(
        now.is_same_owner(committed),
        "after a retire-pending switch the registration names the CANDIDATE, not the incumbent"
    );
    Ok(())
}

#[test]
fn a_retirement_that_failed_is_also_a_success_with_warning() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;

    let candidate = attributed(owner("inst-b", 2, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let discovered = controller.discover()?;
    let prepared = controller.prepare(&discovered, &candidate)?;
    let receipt = controller.commit(
        prepared,
        Retirement::Failed {
            reason: "the incumbent refused SIGTERM".to_owned(),
        },
    )?;

    assert_eq!(
        receipt.outcome(),
        ReplacementOutcome::ReplacedRetirePending,
        "a post-commit retirement that FAILED is still a replacement with a pending retire, not a \
         rollback"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// CAS loss and concurrent starters
// ---------------------------------------------------------------------------

#[test]
fn two_starters_racing_produce_exactly_one_winner_and_a_typed_lost_race() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;

    // Both starters read the SAME incumbent bytes before either commits. This
    // is the interleaving the compare-and-swap exists to resolve.
    let first_view = controller.discover()?;
    let second_view = controller.discover()?;

    let first = attributed(owner("inst-b", 2, 5353, 8081).ok_or("first candidate")?)
        .ok_or("first observation")?;
    let second = attributed(owner("inst-c", 2, 6464, 8082).ok_or("second candidate")?)
        .ok_or("second observation")?;

    let first_prepared = controller.prepare(&first_view, &first)?;
    let second_prepared = controller.prepare(&second_view, &second)?;

    let first_receipt = controller.commit(first_prepared, Retirement::Retired)?;
    let second_receipt = controller.commit(second_prepared, Retirement::Retired)?;

    assert_eq!(
        first_receipt.outcome(),
        ReplacementOutcome::Replaced,
        "the starter that committed first must win outright"
    );
    assert_eq!(
        second_receipt.outcome(),
        ReplacementOutcome::LostRace,
        "the starter whose compare saw the winner's write must lose the race, not overwrite it"
    );
    assert!(
        second_receipt.committed_owner().is_none(),
        "a lost race names no committed owner, because nothing of its own was committed"
    );

    let observed = second_receipt
        .observed()
        .ok_or("a lost race must carry what was actually in the slot")?;
    let winner = RegistrationRecord::decode(observed)?;
    let committed = first_receipt.committed_owner().ok_or("winner owner")?;
    assert!(
        winner.owner().is_same_owner(committed),
        "the bytes the loser observed must identify the winner, so the loser can name it rather \
         than guess"
    );

    let now = registered(&controller)?.ok_or("the slot must name an owner")?;
    assert!(
        now.is_same_owner(committed),
        "after the race the registration must still be the winner's, untouched by the loser"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Rollback linearizability — master law 3
// ---------------------------------------------------------------------------

#[test]
fn a_rollback_leaves_the_slot_byte_identical_and_the_incumbent_owning_it() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent.clone())?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;

    let before = store
        .read_registration()?
        .ok_or("the slot starts occupied")?;

    let candidate = attributed(owner("inst-b", 2, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let discovered = controller.discover()?;
    let prepared = controller.prepare(&discovered, &candidate)?;
    let receipt = prepared.rollback();

    assert_eq!(
        receipt.outcome(),
        ReplacementOutcome::RolledBack,
        "abandoning a candidate before the commit is a rollback"
    );
    let remaining = receipt
        .incumbent()
        .ok_or("a rollback over an occupied slot names the incumbent that kept it")?;
    assert!(
        remaining.is_same_owner(&incumbent),
        "the incumbent named by the rollback receipt must be the one that held the slot"
    );

    let after = store
        .read_registration()?
        .ok_or("the slot must still be occupied")?;
    assert_eq!(
        after, before,
        "a pre-commit rollback must leave the registration's BYTES untouched: the incumbent \
         remains owner by construction, not by a compensating write that could itself fail"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// FailedAfterCommit is not RolledBack
// ---------------------------------------------------------------------------

#[test]
fn a_candidate_that_failed_after_commit_is_still_the_current_registration() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;

    let candidate = attributed(owner("inst-b", 2, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let discovered = controller.discover()?;
    let prepared = controller.prepare(&discovered, &candidate)?;
    let switched = controller.commit(prepared, Retirement::Retired)?;
    let committed = switched
        .committed_owner()
        .ok_or("the switch must have committed")?
        .clone();

    let failed = switched.candidate_failed_after_commit()?;
    assert_eq!(
        failed.outcome(),
        ReplacementOutcome::FailedAfterCommit,
        "a candidate that failed AFTER becoming current is FailedAfterCommit and never RolledBack"
    );

    let now = registered(&controller)?.ok_or("the slot must still name an owner")?;
    assert!(
        now.is_same_owner(&committed),
        "FailedAfterCommit leaves the current registration identifying the CANDIDATE: restoring \
         the incumbent here would be a second ownership transfer performed on a failure report"
    );
    Ok(())
}

#[test]
fn a_switch_that_lost_its_race_cannot_claim_a_post_commit_failure() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;

    let stale_view = controller.discover()?;
    let winner =
        attributed(owner("inst-b", 2, 5353, 8081).ok_or("winner")?).ok_or("winner observation")?;
    let loser =
        attributed(owner("inst-c", 2, 6464, 8082).ok_or("loser")?).ok_or("loser observation")?;

    let loser_prepared = controller.prepare(&stale_view, &loser)?;
    let winner_prepared = controller.prepare(&controller.discover()?, &winner)?;
    controller.commit(winner_prepared, Retirement::Retired)?;
    let lost = controller.commit(loser_prepared, Retirement::Retired)?;

    assert_eq!(
        lost.outcome(),
        ReplacementOutcome::LostRace,
        "the second commit against stale bytes must lose"
    );
    match lost.candidate_failed_after_commit() {
        Err(ControlRefusal::NotCommitted) => Ok(()),
        other => Err(format!(
            "a switch that never committed reported a post-commit failure instead of refusing: \
             {other:?}"
        )
        .into()),
    }
}

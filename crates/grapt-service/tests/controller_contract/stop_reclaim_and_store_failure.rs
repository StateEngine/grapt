// ---------------------------------------------------------------------------
// Stop
// ---------------------------------------------------------------------------

#[test]
fn stop_releases_the_registered_owners_slot() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent.clone())?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    match controller.stop(&discovered, &incumbent)? {
        StopOutcome::Released { owner, .. } => {
            assert!(
                owner.is_same_owner(&incumbent),
                "the release must name the owner whose slot it freed"
            );
        }
        other => {
            return Err(
                format!("the registered owner could not release its slot: {other:?}").into(),
            )
        }
    }
    assert!(
        store.read_registration()?.is_none(),
        "a released slot must hold no registration at all"
    );
    Ok(())
}

#[test]
fn stop_refuses_to_release_a_slot_held_by_somebody_else() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let stranger = owner("inst-z", 1, 4242, 8080).ok_or("stranger")?;
    match controller.stop(&discovered, &stranger)? {
        StopOutcome::NotTheOwner { .. } => {}
        other => {
            return Err(format!(
                "a process that does not own the slot evicted the registration: {other:?}"
            )
            .into())
        }
    }
    assert!(
        store.read_registration()?.is_some(),
        "a refused release must leave the registration in place"
    );
    Ok(())
}

#[test]
fn stop_on_an_empty_slot_is_already_empty_rather_than_a_release() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;
    let anybody = owner("inst-a", 0, 4242, 8080).ok_or("owner")?;

    match controller.stop(&discovered, &anybody)? {
        StopOutcome::SlotAlreadyEmpty => Ok(()),
        other => Err(format!(
            "releasing an empty slot reported something other than already-empty: {other:?}"
        )
        .into()),
    }
}

#[test]
fn stop_that_lost_its_race_leaves_the_new_owner_registered() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent.clone())?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;

    // Discover, then let somebody else take the slot before the release lands.
    let stale_view = controller.discover()?;
    let successor = owner("inst-b", 2, 5353, 8081).ok_or("successor")?;
    let successor_record = record(successor.clone(), 2_000).ok_or("successor record")?;
    store.seed_isolated(Some(successor_record.encode()))?;

    match controller.stop(&stale_view, &incumbent)? {
        StopOutcome::LostRace { .. } => {}
        other => {
            return Err(format!(
                "a release against stale bytes evicted the NEW owner instead of losing: {other:?}"
            )
            .into())
        }
    }
    let now = registered(&controller)?.ok_or("the slot must still name an owner")?;
    assert!(
        now.is_same_owner(&successor),
        "the owner that took the slot after the discovery read must still hold it"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Bounded crash recovery
// ---------------------------------------------------------------------------

#[test]
fn an_unreadable_registration_refuses_an_ordinary_replacement() -> R {
    let store = ElectedControllerAuthority::isolated();
    store.seed_isolated(Some(b"this is not a registration\n".to_vec()))?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    assert!(
        matches!(discovered.state(), SlotState::Unreadable(_)),
        "bytes that decode to no record must be their own slot state, neither empty nor an owner"
    );

    let candidate = attributed(owner("inst-b", 0, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    match controller.prepare(&discovered, &candidate) {
        Err(ControlRefusal::UnreadableRegistration { .. }) => Ok(()),
        other => Err(format!(
            "an ordinary replacement was admitted against a slot whose prior generation is \
             unknowable: {other:?}"
        )
        .into()),
    }
}

#[test]
fn reclaiming_an_unreadable_slot_restarts_ownership_and_records_why() -> R {
    let store = ElectedControllerAuthority::isolated();
    store.seed_isolated(Some(b"torn\n".to_vec()))?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let candidate = attributed(owner("inst-b", 0, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let prepared = controller.reclaim(&discovered, &candidate)?;
    assert!(
        matches!(
            prepared.provenance(),
            grapt_service::Provenance::Reclaimed { .. }
        ),
        "a reclaim must record that ownership restarted, and why"
    );
    let receipt = controller.commit(prepared, Retirement::NoIncumbent)?;
    assert_eq!(
        receipt.outcome(),
        ReplacementOutcome::Replaced,
        "a reclaim that won its compare is a completed replacement"
    );
    let committed = receipt.committed_owner().ok_or("committed owner")?;
    assert_eq!(
        committed.generation(),
        ServiceGeneration::FIRST,
        "ownership of a slot whose prior generation is unreadable restarts at the first"
    );
    Ok(())
}

#[test]
fn a_reclaim_still_loses_to_a_slot_that_was_repaired_first() -> R {
    // Reclaim relaxes the generation, not the compare-and-swap. A slot that was
    // retaken between the discovery read and the reclaim must not be
    // overwritten just because it looked corrupt a moment ago.
    let store = ElectedControllerAuthority::isolated();
    store.seed_isolated(Some(b"torn\n".to_vec()))?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let repaired = owner("inst-r", 0, 7777, 8099).ok_or("repaired")?;
    let repaired_record = record(repaired.clone(), 2_000).ok_or("repaired record")?;
    store.seed_isolated(Some(repaired_record.encode()))?;

    let candidate = attributed(owner("inst-b", 0, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let prepared = controller.reclaim(&discovered, &candidate)?;
    let receipt = controller.commit(prepared, Retirement::NoIncumbent)?;

    assert_eq!(
        receipt.outcome(),
        ReplacementOutcome::LostRace,
        "a reclaim against bytes that are no longer there must lose its race"
    );
    let now = registered(&controller)?.ok_or("the slot must name an owner")?;
    assert!(
        now.is_same_owner(&repaired),
        "the owner that repaired the slot must keep it"
    );
    Ok(())
}

#[test]
fn reclaim_refuses_a_slot_that_is_merely_readable_or_empty() -> R {
    let clock = FixedClock(7_000);
    let candidate = attributed(owner("inst-b", 0, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;

    // Empty.
    let empty = ElectedControllerAuthority::isolated();
    let controller_empty = controller(&empty, &clock)?;
    let discovered = controller_empty.discover()?;
    match controller_empty.reclaim(&discovered, &candidate) {
        Err(ControlRefusal::NothingToReclaim) => {}
        other => {
            return Err(format!(
                "reclaim was admitted against an EMPTY slot, which has an ordinary claim path: \
                 {other:?}"
            )
            .into())
        }
    }

    // Readable.
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let occupied = store_holding(incumbent)?;
    let controller_occupied = controller(&occupied, &clock)?;
    let discovered = controller_occupied.discover()?;
    match controller_occupied.reclaim(&discovered, &candidate) {
        Err(ControlRefusal::NothingToReclaim) => Ok(()),
        other => Err(format!(
            "reclaim was admitted against a READABLE registration, which would let a candidate \
             skip the generation successor rule: {other:?}"
        )
        .into()),
    }
}

#[test]
fn a_write_that_fails_after_its_compare_is_never_reported_as_committed() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;

    let candidate = attributed(owner("inst-a", 0, 4242, 8080).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let discovered = controller.discover()?;
    let prepared = controller.prepare(&discovered, &candidate)?;

    store.fail_next_isolated_write()?;
    match controller.commit(prepared, Retirement::NoIncumbent) {
        Err(ControlRefusal::Store(_)) => {}
        other => {
            return Err(format!(
                "a write that failed after its compare succeeded was not surfaced as a store \
                 failure: {other:?}"
            )
            .into())
        }
    }
    assert!(
        store.read_registration()?.is_none(),
        "a failed write must leave the slot exactly as it was, holding no registration"
    );
    Ok(())
}

#[test]
fn a_slot_whose_generation_cannot_advance_is_refused_rather_than_wrapped() -> R {
    // Ownership transfer advances the generation and never rewinds, so a
    // counter that cannot advance must refuse. Wrapping to zero would make the
    // next candidate look like the first owner this slot ever had.
    let incumbent = owner("inst-a", u64::MAX, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let candidate = attributed(owner("inst-b", 0, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    match controller.prepare(&discovered, &candidate) {
        Err(ControlRefusal::GenerationExhausted) => Ok(()),
        other => Err(format!(
            "a slot at the last representable generation admitted a successor, which would wrap \
             ownership back to the first: {other:?}"
        )
        .into()),
    }
}

#[test]
fn a_candidate_prepared_for_one_slot_cannot_be_committed_into_another() -> R {
    // `prepare` binds the candidate to the controller that admitted it. Without
    // this, a candidate proven ready against slot A's incumbent could be
    // committed into slot B, carrying A's generation and A's compare bytes.
    let store_a = ElectedControllerAuthority::isolated();
    let store_b = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);

    let controller_a = controller(&store_a, &clock)?;
    let controller_b = ServiceController::new(
        ServiceSlotId::new("slot-other").ok_or("other slot")?,
        environment().ok_or("environment")?,
        &store_b,
        &clock,
    );

    let candidate = attributed(owner("inst-a", 0, 4242, 8080).ok_or("candidate")?)
        .ok_or("candidate observation")?;
    let prepared = controller_a.prepare(&controller_a.discover()?, &candidate)?;

    match controller_b.commit(prepared, Retirement::NoIncumbent) {
        Err(ControlRefusal::PreparedForAnotherSlot { .. }) => {}
        other => {
            return Err(format!(
                "a candidate prepared against one slot was committed into another: {other:?}"
            )
            .into())
        }
    }
    assert!(
        store_b.read_registration()?.is_none(),
        "the refused commit must have written nothing into the second slot"
    );
    Ok(())
}

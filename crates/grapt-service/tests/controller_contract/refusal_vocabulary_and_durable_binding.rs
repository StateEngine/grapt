#[test]
fn every_own_refusal_label_maps_from_exactly_one_distinct_variant() -> R {
    // The R4D-2A review's lesson, applied here: a hand-written list of labels
    // lets a new variant ship with a label and no coverage. `label()` matches
    // exhaustively, so a new refusal is a compile error there first; this test
    // closes the other half by proving the declared array and the real mapping
    // agree, with no variant sharing a spelling.
    let variants = [
        ControlRefusal::ProbeAttributedAnotherOwner {
            declared: "a".to_owned(),
            attributed: "b".to_owned(),
        },
        ControlRefusal::PidDoesNotMatchTheProbe {
            declared: 1,
            attributed: 2,
        },
        ControlRefusal::EndpointDoesNotMatchTheProbe {
            declared: "a".to_owned(),
            attributed: "b".to_owned(),
        },
        ControlRefusal::EndpointNotIndependent {
            endpoint: "http://127.0.0.1:1".to_owned(),
        },
        ControlRefusal::CandidateIsTheIncumbent,
        ControlRefusal::NotThisSlot {
            expected: "a".to_owned(),
            declared: "b".to_owned(),
        },
        ControlRefusal::NotTheExpectedEnvironment {
            expected: "a".to_owned(),
            declared: "b".to_owned(),
        },
        ControlRefusal::DurableSlotMismatch {
            controller: "a".to_owned(),
            registered: "b".to_owned(),
        },
        ControlRefusal::DurableEnvironmentMismatch {
            controller: "a".to_owned(),
            registered: "b".to_owned(),
        },
        ControlRefusal::GenerationNotSuccessor {
            declared: 1,
            required: 2,
        },
        ControlRefusal::GenerationExhausted,
        ControlRefusal::UnreadableRegistration {
            source: grapt_service::RegistrationDecodeError::NotUtf8,
        },
        ControlRefusal::NothingToReclaim,
        ControlRefusal::PreparedForAnotherSlot {
            prepared: "a".to_owned(),
            controller: "b".to_owned(),
        },
        ControlRefusal::NotCommitted,
        ControlRefusal::RegistrationBuild(grapt_service::RegistrationBuildError::RecordTooLong {
            maximum: 1,
            found: 2,
        }),
        ControlRefusal::Store(grapt_service::StoreError::Poisoned),
    ];
    let mut produced: Vec<&'static str> = variants.iter().map(ControlRefusal::label).collect();
    let before = produced.len();
    produced.sort_unstable();
    produced.dedup();
    assert_eq!(
        produced.len(),
        before,
        "two refusal variants share one label, so a projection could not tell which law fired"
    );

    let mut declared: Vec<&'static str> = ControlRefusal::OWN_LABELS.to_vec();
    declared.sort_unstable();
    assert_eq!(
        produced, declared,
        "OWN_LABELS and the real label mapping disagree: a variant is either declared without \
         being produced, or produced without being declared"
    );
    Ok(())
}

#[test]
fn no_control_label_collides_with_a_landed_attachment_label() -> R {
    // This lane must not mint a second word for a law R4D-1 already named. The
    // `Attachment` arm delegates, so the only way a collision could appear is a
    // new OWN label spelled like a landed one.
    let landed = [
        AttachmentRefusal::Unattributed,
        AttachmentRefusal::NotTheRecordedOwner,
        AttachmentRefusal::SameOwnerOnBothSides,
        AttachmentRefusal::SameSelectionOnBothSides,
    ];
    for refusal in &landed {
        let landed_label = refusal.label();
        assert!(
            !ControlRefusal::OWN_LABELS.contains(&landed_label),
            "this lane declares its own label {landed_label:?}, which R4D-1 already owns: one \
             law would then have two spellings"
        );
        // And the delegation actually carries the landed word through.
        let carried = ControlRefusal::Attachment(refusal.clone());
        assert_eq!(
            carried.label(),
            landed_label,
            "the Attachment arm must carry R4D-1's own spelling rather than re-naming it"
        );
    }
    Ok(())
}

#[test]
fn a_stale_same_owner_generation_cannot_release_the_current_generation() -> R {
    let current = owner("inst-a", 2, 42, 8080).ok_or("current")?;
    let stale = owner("inst-a", 1, 41, 8079).ok_or("stale")?;
    assert!(current.is_same_owner(&stale));
    let store = store_holding(current.clone())?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;
    assert!(matches!(
        controller.stop(&discovered, &stale)?,
        StopOutcome::NotTheOwner { registered } if registered == current
    ));
    assert_eq!(registered(&controller)?, Some(current));
    Ok(())
}

#[test]
fn authenticated_health_must_attribute_the_declared_candidate_endpoint() -> R {
    let declared = owner("inst-a", 0, 42, 8080).ok_or("declared")?;
    let attributed = owner("inst-a", 0, 42, 8081).ok_or("attributed")?;
    assert!(declared.is_same_owner(&attributed));
    let candidate = Observation::new(
        declared,
        ProbeOutcome::Attributed(attributed),
        version().ok_or("version")?,
    );
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    match controller.prepare(&controller.discover()?, &candidate) {
        Err(ControlRefusal::EndpointDoesNotMatchTheProbe { .. }) => Ok(()),
        other => Err(format!("endpoint-mismatched attribution was admitted: {other:?}").into()),
    }
}

#[test]
fn discovery_refuses_a_durable_record_bound_to_another_slot_or_environment() -> R {
    for (foreign, expected_label) in [
        (
            owner_in("inst-a", 1, 42, 8080, "slot-other", ENVIRONMENT).ok_or("slot record")?,
            "durable-slot-mismatch",
        ),
        (
            owner_in("inst-a", 1, 42, 8080, SLOT, "env-other").ok_or("environment record")?,
            "durable-environment-mismatch",
        ),
    ] {
        let store = ElectedControllerAuthority::isolated();
        store.seed_isolated(Some(record(foreign, 1).ok_or("record")?.encode()))?;
        let clock = FixedClock(7_000);
        let controller = controller(&store, &clock)?;
        let refusal = match controller.discover() {
            Err(refusal) => refusal,
            Ok(discovered) => {
                return Err(
                    format!("foreign durable binding was accepted as {discovered:?}").into(),
                )
            }
        };
        assert_eq!(refusal.label(), expected_label);
    }
    Ok(())
}

#[test]
fn ensure_never_reuses_when_authenticated_observation_version_disagrees() -> R {
    let incumbent = owner("inst-a", 1, 42, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent.clone())?;
    let observation = attributed_version(incumbent, "0.2.0").ok_or("observation")?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    assert!(matches!(
        controller.ensure(
            &controller.discover()?,
            Some(&observation),
            &version().ok_or("version")?,
        ),
        EnsureOutcome::ReplacementRequired {
            reason: ReplacementReason::ObservedVersionMismatch { .. }
        }
    ));
    Ok(())
}

#[test]
fn post_write_unlock_failure_is_a_committed_warning_and_never_a_store_failure() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let candidate =
        attributed(owner("inst-a", 0, 42, 8080).ok_or("candidate")?).ok_or("observation")?;
    let prepared = controller.prepare(&controller.discover()?, &candidate)?;
    store.fail_next_unlock()?;
    let receipt = controller.commit(prepared, Retirement::NoIncumbent)?;
    assert_eq!(receipt.outcome(), ReplacementOutcome::Replaced);
    assert!(receipt.unlock_warning().is_some());
    assert_eq!(registered(&controller)?, receipt.committed_owner().cloned());
    Ok(())
}

#[test]
fn a_stale_preparation_loses_its_compare_and_never_writes() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let view = controller.discover()?;
    let candidate =
        attributed(owner("inst-a", 0, 7, 8080).ok_or("candidate")?).ok_or("observation")?;
    let prepared = controller.prepare(&view, &candidate)?;
    store.seed_isolated(Some(b"actual\n".to_vec()))?;
    let receipt = controller.commit(prepared, Retirement::NoIncumbent)?;
    assert_eq!(receipt.outcome(), ReplacementOutcome::LostRace);
    assert_eq!(receipt.observed(), Some(b"actual\n".as_slice()));
    assert_eq!(
        store.read_registration()?.as_deref(),
        Some(b"actual\n".as_slice()),
        "a lost compare must leave the slot byte-identical"
    );
    Ok(())
}

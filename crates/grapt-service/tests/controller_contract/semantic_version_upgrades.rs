// ---------------------------------------------------------------------------
// Distinct upgrade transition — final R4D-2C law closure
// ---------------------------------------------------------------------------

#[test]
fn a_strictly_greater_semantic_version_commits_as_a_typed_upgrade() -> R {
    let incumbent = owner("inst-a", 4, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "1.2.3+incumbent-build")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(
        owner("inst-b", 5, 5353, 8081).ok_or("candidate")?,
        "1.3.0+candidate-build",
    )
    .ok_or("candidate observation")?;

    let discovered = controller.discover()?;
    let prepared = controller.prepare_upgrade(&discovered, &candidate)?;
    assert_eq!(
        prepared.incumbent_version().as_str(),
        "1.2.3+incumbent-build"
    );
    assert_eq!(
        prepared.candidate_version().as_str(),
        "1.3.0+candidate-build"
    );
    let receipt = controller.commit_upgrade(prepared, Retirement::TimedOut)?;

    assert_eq!(receipt.outcome(), ReplacementOutcome::ReplacedRetirePending);
    assert_eq!(
        receipt.incumbent_version().as_str(),
        "1.2.3+incumbent-build"
    );
    assert_eq!(
        receipt.candidate_version().as_str(),
        "1.3.0+candidate-build"
    );
    let written = RegistrationRecord::decode(&store.read_registration()?.ok_or("registration")?)?;
    assert_eq!(
        written.version().as_str(),
        "1.3.0+candidate-build",
        "semantic precedence admits the transition, but durable discovery preserves the exact stored string"
    );
    Ok(())
}

#[test]
fn the_typed_upgrade_receipt_preserves_the_exact_candidate_version_string() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "1.2.3+incumbent")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(
        owner("inst-b", 2, 5353, 8081).ok_or("candidate")?,
        "1.3.0+candidate-exact",
    )
    .ok_or("candidate observation")?;
    let prepared = controller.prepare_upgrade(&controller.discover()?, &candidate)?;
    let receipt = controller.commit_upgrade(prepared, Retirement::Retired)?;
    assert_eq!(
        receipt.candidate_version().as_str(),
        "1.3.0+candidate-exact"
    );
    Ok(())
}

#[test]
fn the_typed_upgrade_receipt_preserves_the_exact_incumbent_version_string() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "1.2.3+incumbent-exact")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(
        owner("inst-b", 2, 5353, 8081).ok_or("candidate")?,
        "1.3.0+candidate",
    )
    .ok_or("candidate observation")?;
    let prepared = controller.prepare_upgrade(&controller.discover()?, &candidate)?;
    let receipt = controller.commit_upgrade(prepared, Retirement::Retired)?;
    assert_eq!(
        receipt.incumbent_version().as_str(),
        "1.2.3+incumbent-exact"
    );
    Ok(())
}

#[test]
fn upgrade_commit_preserves_the_retirement_observation() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "1.2.3")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(owner("inst-b", 2, 5353, 8081).ok_or("candidate")?, "1.3.0")
        .ok_or("candidate observation")?;
    let prepared = controller.prepare_upgrade(&controller.discover()?, &candidate)?;
    let receipt = controller.commit_upgrade(prepared, Retirement::TimedOut)?;
    assert_eq!(receipt.switch_receipt().retirement(), &Retirement::TimedOut);
    assert_eq!(receipt.outcome(), ReplacementOutcome::ReplacedRetirePending);
    Ok(())
}

#[test]
fn a_build_metadata_only_change_is_equal_precedence_not_an_upgrade() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "1.2.3+left")?;
    let before = store.read_registration()?.ok_or("registration")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(
        owner("inst-b", 2, 5353, 8081).ok_or("candidate")?,
        "1.2.3+right",
    )
    .ok_or("candidate observation")?;

    let refusal = controller
        .prepare_upgrade(&controller.discover()?, &candidate)
        .err()
        .ok_or("equal precedence must refuse")?;
    match &refusal {
        UpgradeTransitionRefusal::SamePrecedence {
            incumbent,
            candidate,
        } => {
            assert_eq!(incumbent.as_str(), "1.2.3+left");
            assert_eq!(candidate.as_str(), "1.2.3+right");
        }
        other => return Err(format!("wrong refusal for build-only change: {other:?}").into()),
    }
    assert_eq!(refusal.label(), "upgrade-same-precedence");
    assert_eq!(
        store.read_registration()?.as_deref(),
        Some(before.as_slice())
    );
    Ok(())
}

#[test]
fn a_lower_candidate_is_a_named_downgrade_refusal() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "2.0.0")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate =
        attributed_version(owner("inst-b", 2, 5353, 8081).ok_or("candidate")?, "1.99.0")
            .ok_or("candidate observation")?;
    let refusal = controller
        .prepare_upgrade(&controller.discover()?, &candidate)
        .err()
        .ok_or("downgrade must refuse")?;
    assert!(matches!(
        &refusal,
        UpgradeTransitionRefusal::Downgrade { .. }
    ));
    assert_eq!(refusal.label(), "upgrade-downgrade");
    Ok(())
}

#[test]
fn an_unorderable_incumbent_remains_a_side_specific_typed_refusal() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "nightly-2026-08-14")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(owner("inst-b", 2, 5353, 8081).ok_or("candidate")?, "2.0.0")
        .ok_or("candidate observation")?;
    let refusal = controller
        .prepare_upgrade(&controller.discover()?, &candidate)
        .err()
        .ok_or("unorderable incumbent must refuse")?;
    assert!(matches!(
        &refusal,
        UpgradeTransitionRefusal::Version(VersionUpgradeRefusal::Incumbent(
            VersionOrderError::MalformedCore { .. }
        ))
    ));
    assert_eq!(refusal.label(), "upgrade-incumbent-version-unorderable");
    Ok(())
}

#[test]
fn an_unorderable_candidate_remains_a_side_specific_typed_refusal() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "1.0.0")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(owner("inst-b", 2, 5353, 8081).ok_or("candidate")?, "2.0")
        .ok_or("candidate observation")?;
    let refusal = controller
        .prepare_upgrade(&controller.discover()?, &candidate)
        .err()
        .ok_or("unorderable candidate must refuse")?;
    assert!(matches!(
        &refusal,
        UpgradeTransitionRefusal::Version(VersionUpgradeRefusal::Candidate(
            VersionOrderError::MalformedCore { .. }
        ))
    ));
    assert_eq!(refusal.label(), "upgrade-candidate-version-unorderable");
    Ok(())
}

#[test]
fn upgrade_requires_a_readable_incumbent() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(owner("inst-b", 0, 5353, 8081).ok_or("candidate")?, "1.0.0")
        .ok_or("candidate observation")?;
    let refusal = controller
        .prepare_upgrade(&controller.discover()?, &candidate)
        .err()
        .ok_or("empty slot must refuse upgrade")?;
    assert!(matches!(&refusal, UpgradeTransitionRefusal::NoIncumbent));
    assert_eq!(refusal.label(), "upgrade-no-incumbent");
    Ok(())
}

#[test]
fn upgrade_does_not_turn_an_unreadable_registration_into_absence() -> R {
    let store = ElectedControllerAuthority::isolated();
    store.seed_isolated(Some(b"torn\r\n".to_vec()))?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(owner("inst-b", 0, 5353, 8081).ok_or("candidate")?, "1.0.0")
        .ok_or("candidate observation")?;
    let refusal = controller
        .prepare_upgrade(&controller.discover()?, &candidate)
        .err()
        .ok_or("unreadable slot must refuse upgrade")?;
    assert!(matches!(
        &refusal,
        UpgradeTransitionRefusal::Control(ControlRefusal::UnreadableRegistration { .. })
    ));
    assert_eq!(refusal.label(), "unreadable-registration");
    Ok(())
}

#[test]
fn prerelease_numeric_identifiers_use_semantic_not_byte_order() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "1.0.0-rc.9")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(
        owner("inst-b", 2, 5353, 8081).ok_or("candidate")?,
        "1.0.0-rc.10",
    )
    .ok_or("candidate observation")?;
    let prepared = controller.prepare_upgrade(&controller.discover()?, &candidate)?;
    let receipt = controller.commit_upgrade(prepared, Retirement::Retired)?;
    assert_eq!(receipt.outcome(), ReplacementOutcome::Replaced);
    Ok(())
}

#[test]
fn upgrade_preserves_candidate_precondition_refusals() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "1.0.0")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate =
        attributed_version(owner("inst-b", 99, 5353, 8081).ok_or("candidate")?, "2.0.0")
            .ok_or("candidate observation")?;
    let refusal = controller
        .prepare_upgrade(&controller.discover()?, &candidate)
        .err()
        .ok_or("wrong generation must refuse")?;
    assert!(matches!(
        &refusal,
        UpgradeTransitionRefusal::Control(ControlRefusal::GenerationNotSuccessor { .. })
    ));
    assert_eq!(refusal.label(), "generation-not-successor");
    Ok(())
}

#[test]
fn an_upgrade_that_loses_its_cas_reports_lost_race_without_writing() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "1.0.0")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(owner("inst-b", 2, 5353, 8081).ok_or("candidate")?, "2.0.0")
        .ok_or("candidate observation")?;
    let prepared = controller.prepare_upgrade(&controller.discover()?, &candidate)?;
    store.seed_isolated(Some(b"winner\n".to_vec()))?;
    let receipt = controller.commit_upgrade(prepared, Retirement::Retired)?;
    assert_eq!(receipt.outcome(), ReplacementOutcome::LostRace);
    assert_eq!(
        store.read_registration()?.as_deref(),
        Some(b"winner\n".as_slice())
    );
    assert_eq!(receipt.incumbent_version().as_str(), "1.0.0");
    assert_eq!(receipt.candidate_version().as_str(), "2.0.0");
    Ok(())
}

#[test]
fn a_committed_upgrade_can_report_failure_without_claiming_rollback() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding_version(incumbent, "1.0.0")?;
    let clock = FixedClock(8_000);
    let controller = controller(&store, &clock)?;
    let candidate = attributed_version(owner("inst-b", 2, 5353, 8081).ok_or("candidate")?, "2.0.0")
        .ok_or("candidate observation")?;
    let prepared = controller.prepare_upgrade(&controller.discover()?, &candidate)?;
    let receipt = controller
        .commit_upgrade(prepared, Retirement::Retired)?
        .candidate_failed_after_commit()?;
    assert_eq!(receipt.outcome(), ReplacementOutcome::FailedAfterCommit);
    let current = registered(&controller)?.ok_or("current owner")?;
    assert!(receipt
        .committed_owner()
        .ok_or("committed upgrade owner")?
        .is_same_owner(&current));
    Ok(())
}

// ---------------------------------------------------------------------------
// Stale registration, PID reuse and rebind
// ---------------------------------------------------------------------------

#[test]
fn a_registration_no_probe_attributed_is_stale_rather_than_healthy() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent.clone())?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;
    let required = version().ok_or("version")?;

    // A 2xx that named no owner. R4D-1 established that this proves liveness
    // and nothing else, so it cannot keep a registration alive.
    let unattributed = Observation::new(
        incumbent.clone(),
        ProbeOutcome::HealthyUnattributed,
        required.clone(),
    );
    match controller.ensure(&discovered, Some(&unattributed), &required) {
        EnsureOutcome::ReplacementRequired {
            reason: ReplacementReason::StaleRegistration { .. },
        } => {}
        other => {
            return Err(format!(
                "an unattributed health answer kept a registration alive instead of making it \
                 stale: {other:?}"
            )
            .into())
        }
    }

    // And with no probe at all.
    match controller.ensure(&discovered, None, &required) {
        EnsureOutcome::ReplacementRequired {
            reason: ReplacementReason::StaleRegistration { .. },
        } => Ok(()),
        other => Err(format!(
            "a registration nothing probed was treated as healthy instead of stale: {other:?}"
        )
        .into()),
    }
}

#[test]
fn a_reused_pid_does_not_let_a_different_instance_reuse_the_slot() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;
    let required = version().ok_or("version")?;

    // A DIFFERENT instance that inherited the incumbent's PID and endpoint.
    // Everything a `{pid, url}` identity would compare matches; the instance
    // identity does not.
    let impostor = owner("inst-z", 1, 4242, 8080).ok_or("impostor")?;
    let observation = attributed(impostor).ok_or("impostor observation")?;

    match controller.ensure(&discovered, Some(&observation), &required) {
        EnsureOutcome::ReplacementRequired {
            reason: ReplacementReason::StaleRegistration { .. },
        } => Ok(()),
        other => Err(format!(
            "a different instance holding the incumbent's PID and endpoint inherited its \
             registration: {other:?}"
        )
        .into()),
    }
}

#[test]
fn the_same_owner_behind_a_new_pid_and_port_still_holds_its_slot() -> R {
    // The service-rebind row, and the control for the PID test above. A gate
    // that refused every observation would pass that test and be worthless.
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;
    let required = version().ok_or("version")?;

    // Same slot, instance and environment; new PID and new ephemeral port.
    let rebound = owner("inst-a", 1, 9999, 9090).ok_or("rebound")?;
    let observation = attributed(rebound).ok_or("rebound observation")?;

    match controller.ensure(&discovered, Some(&observation), &required) {
        EnsureOutcome::ReusedIncumbent { .. } => Ok(()),
        other => Err(format!(
            "the same owner behind a new PID and ephemeral port lost its slot: {other:?}"
        )
        .into()),
    }
}

#[test]
fn an_incumbent_of_the_wrong_version_is_replaced_rather_than_reused() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent.clone())?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let observation = attributed(incumbent).ok_or("observation")?;
    let required = ServiceRegistrationVersion::new("0.2.0").ok_or("required version")?;
    match controller.ensure(&discovered, Some(&observation), &required) {
        // `IncompatibleVersion` rather than `StaleRegistration` is the whole
        // point, and it is the addendum's law stated as an observation: the
        // owner was RECOGNISED and then found to be the wrong version. Had
        // version been part of identity, a version change would have made the
        // incumbent a stranger instead of a known owner running the wrong build.
        EnsureOutcome::ReplacementRequired {
            reason: ReplacementReason::IncompatibleVersion { registered, .. },
        } => {
            assert_eq!(
                registered.as_str(),
                VERSION,
                "the refusal must report the version actually registered, which means the \
                 registration was read and its owner recognised rather than dismissed as stale"
            );
            Ok(())
        }
        other => Err(format!(
            "a live incumbent of the wrong version was not reported as a version mismatch; if it \
             came back stale, version has leaked into owner identity: {other:?}"
        )
        .into()),
    }
}

// ---------------------------------------------------------------------------
// The five pre-switch conditions — master law 1
// ---------------------------------------------------------------------------

#[test]
fn an_unattributed_candidate_probe_is_refused_with_r4d1s_own_vocabulary() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let declared = owner("inst-a", 0, 4242, 8080).ok_or("candidate")?;
    let candidate = Observation::new(
        declared,
        ProbeOutcome::HealthyUnattributed,
        version().ok_or("version")?,
    );

    match controller.prepare(&discovered, &candidate) {
        Err(ControlRefusal::Attachment(refusal)) => {
            assert_eq!(
                refusal.label(),
                "unattributed",
                "the attachment law must be carried through R4D-1's own refusal, not re-spelled"
            );
            Ok(())
        }
        other => Err(format!(
            "a candidate whose probe named no owner was admitted for a switch: {other:?}"
        )
        .into()),
    }
}

#[test]
fn a_probe_that_attributed_another_owner_is_refused() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let declared = owner("inst-a", 0, 4242, 8080).ok_or("declared")?;
    let answered = owner("inst-other", 0, 4242, 8080).ok_or("answered")?;
    let candidate = Observation::new(
        declared,
        ProbeOutcome::Attributed(answered),
        version().ok_or("version")?,
    );

    match controller.prepare(&discovered, &candidate) {
        Err(ControlRefusal::ProbeAttributedAnotherOwner { .. }) => Ok(()),
        other => Err(format!(
            "a candidate whose health check reached a DIFFERENT engine was admitted: {other:?}"
        )
        .into()),
    }
}

#[test]
fn owner_identity_matching_does_not_excuse_a_mismatched_process_id() -> R {
    // `OwnerReceipt::is_same_owner` deliberately excludes the PID, so identity
    // can match while the process does not. Master law 1 requires a matching
    // instance PID *alongside* identity, and this is the case that separates
    // the two checks: without it, the PID condition would be unwitnessed.
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let declared = owner("inst-a", 0, 4242, 8080).ok_or("declared")?;
    let answered = owner("inst-a", 0, 9999, 8080).ok_or("answered")?;
    assert!(
        declared.is_same_owner(&answered),
        "the two receipts must be the SAME owner, or this test would be proving the identity \
         check rather than the PID check"
    );
    let candidate = Observation::new(
        declared,
        ProbeOutcome::Attributed(answered),
        version().ok_or("version")?,
    );

    match controller.prepare(&discovered, &candidate) {
        Err(ControlRefusal::PidDoesNotMatchTheProbe {
            declared: d,
            attributed: a,
        }) => {
            assert_eq!(
                (d, a),
                (4242, 9999),
                "the refusal must name both the declared and the attributed process"
            );
            Ok(())
        }
        other => Err(format!(
            "a candidate whose declared PID differed from the process that answered was \
             admitted: {other:?}"
        )
        .into()),
    }
}

#[test]
fn a_candidate_on_the_incumbents_endpoint_is_refused() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    // Same endpoint as the incumbent: no independent ephemeral port.
    let candidate = attributed(owner("inst-b", 2, 5353, 8080).ok_or("candidate")?)
        .ok_or("candidate observation")?;

    match controller.prepare(&discovered, &candidate) {
        Err(ControlRefusal::EndpointNotIndependent { .. }) => Ok(()),
        other => Err(format!(
            "a candidate sharing the incumbent's endpoint was admitted, so a failed candidate \
             would leave the incumbent's address serving nothing: {other:?}"
        )
        .into()),
    }
}

#[test]
fn the_incumbent_cannot_replace_itself() -> R {
    let incumbent = owner("inst-a", 1, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent.clone())?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    // The candidate is the incumbent BY IDENTITY — same slot, instance and
    // environment — but is deliberately given a different endpoint and the
    // successor generation.
    //
    // Handing this test the incumbent verbatim is the obvious thing to write
    // and it is wrong. It also shares the incumbent's endpoint and generation,
    // so `prepare` has three reasons to refuse and only one of them is the law
    // under test. Scored that way, a mutant that removed the identity check
    // still reddened this witness — through `EndpointNotIndependent`, from a
    // law this test does not own. The witness fired and attributed nothing.
    //
    // With the other two conditions made to PASS, the identity check is the
    // only thing left that can refuse.
    let candidate_owner = owner("inst-a", 2, 4242, 8081).ok_or("candidate")?;
    assert!(
        incumbent.is_same_owner(&candidate_owner),
        "the candidate must be the SAME owner, or this test proves nothing about self-replacement"
    );
    assert_ne!(
        incumbent.endpoint(),
        candidate_owner.endpoint(),
        "the endpoints must DIFFER, or the endpoint-independence guard can refuse in place of the \
         identity guard and this witness attributes nothing"
    );
    let candidate = attributed(candidate_owner).ok_or("candidate observation")?;
    match controller.prepare(&discovered, &candidate) {
        Err(ControlRefusal::CandidateIsTheIncumbent) => Ok(()),
        other => Err(format!(
            "the registered owner was admitted as a candidate against its own slot: {other:?}"
        )
        .into()),
    }
}

#[test]
fn a_candidate_carrying_another_environments_receipt_is_refused() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let declared = owner_in("inst-a", 0, 4242, 8080, SLOT, "env-beta").ok_or("declared")?;
    let candidate = attributed(declared).ok_or("candidate observation")?;

    match controller.prepare(&discovered, &candidate) {
        Err(ControlRefusal::NotTheExpectedEnvironment { .. }) => Ok(()),
        other => Err(format!(
            "a candidate built for a different environment was admitted: {other:?}"
        )
        .into()),
    }
}

#[test]
fn a_candidate_naming_another_slot_is_refused() -> R {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let declared =
        owner_in("inst-a", 0, 4242, 8080, "slot-other", ENVIRONMENT).ok_or("declared")?;
    let candidate = attributed(declared).ok_or("candidate observation")?;

    match controller.prepare(&discovered, &candidate) {
        Err(ControlRefusal::NotThisSlot { .. }) => Ok(()),
        other => Err(format!(
            "a candidate naming a different slot was registered into this one: {other:?}"
        )
        .into()),
    }
}

#[test]
fn a_candidate_that_declares_a_non_successor_generation_is_refused() -> R {
    let incumbent = owner("inst-a", 3, 4242, 8080).ok_or("incumbent")?;
    let store = store_holding(incumbent)?;
    let clock = FixedClock(7_000);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    // The successor of 3 is 4. A candidate that believes it is generation 9
    // has read something stale, and quietly writing it in as 4 would hide that.
    let candidate = attributed(owner("inst-b", 9, 5353, 8081).ok_or("candidate")?)
        .ok_or("candidate observation")?;

    match controller.prepare(&discovered, &candidate) {
        Err(ControlRefusal::GenerationNotSuccessor { declared, required }) => {
            assert_eq!(
                (declared, required),
                (9, 4),
                "the refusal must name both the generation claimed and the one this slot requires"
            );
            Ok(())
        }
        other => Err(format!(
            "a candidate declaring a non-successor generation was admitted: {other:?}"
        )
        .into()),
    }
}

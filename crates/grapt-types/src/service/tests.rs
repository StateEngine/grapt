use super::*;

#[test]
fn pid_zero_is_unrepresentable() {
    assert!(ProcessId::new(0).is_none());
    assert_eq!(ProcessId::new(29072).map(ProcessId::value), Some(29072));
}

fn receipt(instance: &str, pid: u32, port: u16) -> Option<OwnerReceipt> {
    Some(OwnerReceipt::new(
        ServiceSlotId::new("slot-1")?,
        ServiceInstanceId::new(instance)?,
        EnvironmentReceipt::new("env-1")?,
        ServiceGeneration::new(1),
        ProcessId::new(pid)?,
        Endpoint::new(format!("http://127.0.0.1:{port}"))?,
    ))
}

#[test]
fn a_reused_pid_does_not_inherit_ownership() -> Result<(), Box<dyn std::error::Error>> {
    let incumbent = receipt("inst-1", 4242, 8080).ok_or("incumbent")?;
    let impostor = receipt("inst-2", 4242, 8080).ok_or("impostor")?;
    assert!(
        !incumbent.is_same_owner(&impostor),
        "same PID and endpoint, different instance: not the owner"
    );

    let rebound = receipt("inst-1", 9999, 9090).ok_or("rebound")?;
    assert!(
        incumbent.is_same_owner(&rebound),
        "the same owner keeps the slot across a new PID and ephemeral port"
    );
    Ok(())
}

#[test]
fn an_endpoint_needs_a_scheme() {
    assert!(Endpoint::new("127.0.0.1:8080").is_none());
    assert!(Endpoint::new("").is_none());
    assert!(Endpoint::new("http://127.0.0.1:8080").is_some());
}

#[test]
fn rolled_back_and_failed_after_commit_are_distinct() {
    assert_ne!(
        ReplacementOutcome::RolledBack,
        ReplacementOutcome::FailedAfterCommit
    );
}

fn version(raw: &str) -> Result<ServiceRegistrationVersion, Box<dyn std::error::Error>> {
    ServiceRegistrationVersion::new(raw).ok_or_else(|| format!("{raw} is not storable").into())
}

fn precedence(raw: &str) -> Result<SemanticVersion, Box<dyn std::error::Error>> {
    Ok(version(raw)?.semantic_precedence()?)
}

#[test]
fn an_empty_registration_version_is_unrepresentable() {
    assert!(ServiceRegistrationVersion::new("").is_none());
    assert!(ServiceRegistrationVersion::new("0.1.0").is_some());
}

#[test]
fn the_recorded_value_survives_the_projection() -> Result<(), Box<dyn std::error::Error>> {
    // The oracle's discovery contract stores an opaque string. Projecting
    // it onto a precedence must not rewrite, normalize or canonicalize it.
    let recorded = "1.02.3-Rc.01+Build.7";
    let stored = ServiceRegistrationVersion::new(recorded).ok_or("storable")?;
    assert_eq!(
        stored.as_str(),
        recorded,
        "an unorderable value is still recorded verbatim"
    );
    assert!(
        stored.semantic_precedence().is_err(),
        "and it genuinely has no precedence, so this is not a vacuous check"
    );
    Ok(())
}

#[test]
fn a_free_form_oracle_version_is_still_storable() -> Result<(), Box<dyn std::error::Error>> {
    // `client/src/effect/service.ts` declares the registration version as
    // an optional free-form string. If construction demanded SemVer, the
    // controller could not read this incumbent at all, and so could not
    // report the version mismatch the oracle reports today.
    //
    // Stated as an assertion rather than `?` on purpose: a witness that
    // fails by propagating Err leaves no panic line, so its evidence names
    // no law and the battery records it without attribution.
    let stored = ServiceRegistrationVersion::new("nightly-2026-08-14");
    assert!(
        stored.is_some(),
        "the oracle records an optional free-form string, so construction must not demand SemVer"
    );
    let stored = stored.ok_or("storable")?;
    assert_eq!(stored.as_str(), "nightly-2026-08-14");
    Ok(())
}

#[test]
fn core_fields_order_numerically_not_lexicographically() -> Result<(), Box<dyn std::error::Error>> {
    // The exact case a byte order gets backwards: "0.10.0" sorts BELOW
    // "0.9.0" as text, and above it as a version.
    assert!(
        precedence("0.9.0")? < precedence("0.10.0")?,
        "0.9.0 precedes 0.10.0; a lexicographic order claims the reverse"
    );
    assert!(
        "0.10.0" < "0.9.0",
        "control: the byte order really does disagree, so the check above is load-bearing"
    );
    Ok(())
}

#[test]
fn each_core_field_outranks_the_one_below_it() -> Result<(), Box<dyn std::error::Error>> {
    assert!(
        precedence("1.0.0")? < precedence("2.0.0")?,
        "major dominates"
    );
    assert!(
        precedence("2.1.9")? < precedence("2.2.0")?,
        "minor dominates patch"
    );
    assert!(
        precedence("2.2.1")? < precedence("2.2.2")?,
        "patch breaks the tie"
    );
    Ok(())
}

#[test]
fn a_pre_release_ranks_below_its_own_release() -> Result<(), Box<dyn std::error::Error>> {
    assert!(
        precedence("1.0.0-alpha")? < precedence("1.0.0")?,
        "a pre-release has lower precedence than the release it precedes"
    );
    assert!(
        !precedence("1.0.0")?.is_pre_release(),
        "control: the release side really carries no pre-release"
    );
    assert!(
        precedence("1.0.0-alpha")?.is_pre_release(),
        "control: the pre-release side really carries one"
    );
    Ok(())
}

#[test]
fn a_numeric_pre_release_identifier_ranks_below_an_alphanumeric_one(
) -> Result<(), Box<dyn std::error::Error>> {
    assert!(
        precedence("1.0.0-1")? < precedence("1.0.0-alpha")?,
        "numeric identifiers have lower precedence than alphanumeric ones"
    );
    Ok(())
}

#[test]
fn numeric_pre_release_identifiers_compare_numerically() -> Result<(), Box<dyn std::error::Error>> {
    assert!(
        precedence("1.0.0-alpha.9")? < precedence("1.0.0-alpha.10")?,
        "alpha.9 precedes alpha.10; a byte comparison claims the reverse"
    );
    Ok(())
}

#[test]
fn arbitrary_length_numeric_identifiers_have_semver_precedence(
) -> Result<(), Box<dyn std::error::Error>> {
    let below_u64_boundary = precedence("18446744073709551615.0.0")?;
    let above_u64_boundary = precedence("18446744073709551616.0.0")?;
    let same_width_higher = precedence("18446744073709551617.0.0")?;
    let wider = precedence("100000000000000000000.0.0")?;

    assert_eq!(
        above_u64_boundary.major(),
        "18446744073709551616",
        "the public projection retains every above-u64 decimal digit"
    );
    assert_eq!(above_u64_boundary.minor(), "0");
    assert_eq!(above_u64_boundary.patch(), "0");
    assert!(
        below_u64_boundary < above_u64_boundary,
        "crossing the u64 boundary remains ordinary SemVer precedence"
    );
    assert!(
        above_u64_boundary < same_width_higher,
        "equal-width arbitrary decimals compare by ASCII after their length ties"
    );
    assert!(
        same_width_higher < wider,
        "a longer canonical decimal has greater numeric precedence"
    );

    assert!(
        precedence("1.0.0-18446744073709551616")? < precedence("1.0.0-18446744073709551617")?,
        "numeric pre-release identifiers use the same unbounded decimal order"
    );
    assert_eq!(
        precedence("18446744073709551616.0.0+build.1")?,
        precedence("18446744073709551616.0.0+build.2")?,
        "build metadata remains excluded above the machine-integer boundary"
    );
    assert_eq!(
        ServiceRegistrationVersion::upgrade_disposition(
            &version("18446744073709551616.0.0")?,
            &version("18446744073709551617.0.0")?
        )?,
        UpgradeDisposition::Upgrade,
        "the owned upgrade API accepts the same arbitrary-length precedence"
    );
    Ok(())
}

#[test]
fn a_longer_pre_release_outranks_its_own_prefix() -> Result<(), Box<dyn std::error::Error>> {
    assert!(
        precedence("1.0.0-alpha")? < precedence("1.0.0-alpha.1")?,
        "with every shared identifier equal, the larger set of fields wins"
    );
    Ok(())
}

#[test]
fn the_semver_specification_ordering_holds_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
    // SemVer 2.0.0 §11's own worked example, in order.
    let ordered = [
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-alpha.beta",
        "1.0.0-beta",
        "1.0.0-beta.2",
        "1.0.0-beta.11",
        "1.0.0-rc.1",
        "1.0.0",
    ];
    for window in ordered.windows(2) {
        let (lower, higher) = (window[0], window[1]);
        assert!(
            precedence(lower)? < precedence(higher)?,
            "{lower} must precede {higher}"
        );
    }
    Ok(())
}

#[test]
fn build_metadata_does_not_change_precedence() -> Result<(), Box<dyn std::error::Error>> {
    // Checked before the comparison, and as an assertion rather than `?`,
    // because a build that is never split off makes these values
    // unorderable outright: a witness failing by propagating that Err
    // leaves no panic line and so names no law.
    let carried = version("1.0.0+build.1")?.semantic_precedence();
    assert!(
        carried.is_ok(),
        "build metadata must be split off, not read as part of the core: {carried:?}"
    );
    assert_eq!(
        precedence("1.0.0+build.1")?,
        precedence("1.0.0+build.2")?,
        "SemVer excludes build metadata from precedence"
    );
    assert_eq!(
        precedence("1.0.0")?,
        precedence("1.0.0+build.1")?,
        "and an absent build ranks with any present one"
    );
    Ok(())
}

#[test]
fn a_build_metadata_only_change_is_not_an_upgrade() -> Result<(), Box<dyn std::error::Error>> {
    let incumbent = version("1.0.0+build.1")?;
    let candidate = version("1.0.0+build.2")?;
    assert_eq!(
        ServiceRegistrationVersion::upgrade_disposition(&incumbent, &candidate)?,
        UpgradeDisposition::SamePrecedence,
        "equal precedence is not an upgrade, however different the recorded bytes"
    );
    Ok(())
}

#[test]
fn exact_compatibility_is_separate_from_precedence() -> Result<(), Box<dyn std::error::Error>> {
    // The two rules must be able to disagree, and here they do: the oracle
    // reuses an incumbent only on an exact match, while precedence sees
    // these as the same version.
    let incumbent = version("1.0.0+build.1")?;
    let candidate = version("1.0.0+build.2")?;
    assert!(
        !incumbent.is_exactly(&candidate),
        "exact compatibility keys on the recorded value"
    );
    assert_eq!(
        candidate.semantic_precedence()?,
        incumbent.semantic_precedence()?,
        "while precedence deliberately cannot tell them apart"
    );
    Ok(())
}

#[test]
fn an_exact_match_is_the_same_recorded_version() -> Result<(), Box<dyn std::error::Error>> {
    let incumbent = version("1.0.0")?;
    let same = version("1.0.0")?;
    let other = version("1.0.0+build.1")?;
    assert!(incumbent.is_exactly(&same));
    assert!(
        !incumbent.is_exactly(&other),
        "control: exact compatibility is not satisfied by mere precedence equality"
    );
    Ok(())
}

#[test]
fn a_strictly_greater_candidate_is_an_upgrade() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        ServiceRegistrationVersion::upgrade_disposition(&version("0.9.0")?, &version("0.10.0")?)?,
        UpgradeDisposition::Upgrade
    );
    Ok(())
}

#[test]
fn a_lower_candidate_is_a_distinguishable_downgrade() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        ServiceRegistrationVersion::upgrade_disposition(&version("0.10.0")?, &version("0.9.0")?)?,
        UpgradeDisposition::Downgrade,
        "a downgrade is reported as itself, not as a refusal or a same-precedence tie"
    );
    Ok(())
}

#[test]
fn a_pre_release_candidate_does_not_upgrade_its_release() -> Result<(), Box<dyn std::error::Error>>
{
    assert_eq!(
        ServiceRegistrationVersion::upgrade_disposition(
            &version("1.0.0")?,
            &version("1.0.0-rc.1")?
        )?,
        UpgradeDisposition::Downgrade
    );
    Ok(())
}

#[test]
fn an_unorderable_core_is_refused_with_its_field() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        version("1.x.0")?.semantic_precedence(),
        Err(VersionOrderError::InvalidCoreField {
            field: CoreField::Minor
        })
    );
    Ok(())
}

#[test]
fn a_short_core_is_refused_with_its_field_count() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        version("1.0")?.semantic_precedence(),
        Err(VersionOrderError::MalformedCore { fields: 2 }),
        "a malformed core reports how many fields it actually had"
    );
    assert_eq!(
        version("1.0.0.0")?.semantic_precedence(),
        Err(VersionOrderError::MalformedCore { fields: 4 }),
        "and counts a long core as well as a short one"
    );
    Ok(())
}

#[test]
fn a_leading_zero_core_field_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        version("1.01.0")?.semantic_precedence(),
        Err(VersionOrderError::InvalidCoreField {
            field: CoreField::Minor
        }),
        "a leading-zero core field is not a SemVer numeric identifier"
    );
    assert!(
        version("1.0.0")?.semantic_precedence().is_ok(),
        "control: a bare zero is still a valid field"
    );
    Ok(())
}

#[test]
fn an_empty_pre_release_identifier_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        version("1.0.0-alpha..1")?.semantic_precedence(),
        Err(VersionOrderError::InvalidPreRelease { at: 1 }),
        "an empty pre-release identifier is refused at its own index"
    );
    Ok(())
}

#[test]
fn a_leading_zero_numeric_pre_release_identifier_is_refused(
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        version("1.0.0-alpha.01")?.semantic_precedence(),
        Err(VersionOrderError::InvalidPreRelease { at: 1 }),
        "a leading-zero numeric pre-release identifier is refused"
    );
    assert!(
        version("1.0.0-alpha.0")?.semantic_precedence().is_ok(),
        "control: a bare zero identifier is legal"
    );
    Ok(())
}

#[test]
fn a_malformed_build_is_refused_rather_than_ignored() -> Result<(), Box<dyn std::error::Error>> {
    // Build metadata does not affect precedence, but that is not a licence
    // to accept anything after the `+`.
    assert_eq!(
        version("1.0.0+build..1")?.semantic_precedence(),
        Err(VersionOrderError::InvalidBuild { at: 1 }),
        "an empty build identifier is refused, not silently ignored"
    );
    assert_eq!(
        version("1.0.0+build!")?.semantic_precedence(),
        Err(VersionOrderError::InvalidBuild { at: 0 }),
        "and so is a non-alphanumeric one"
    );
    Ok(())
}

#[test]
fn an_unorderable_incumbent_names_the_incumbent() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        ServiceRegistrationVersion::upgrade_disposition(&version("nightly")?, &version("1.0.0")?),
        Err(UpgradeRefusal::Incumbent(
            VersionOrderError::MalformedCore { fields: 1 }
        )),
        "an unorderable incumbent is a different operational fact from an unorderable candidate"
    );
    Ok(())
}

#[test]
fn an_unorderable_candidate_names_the_candidate() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        ServiceRegistrationVersion::upgrade_disposition(&version("1.0.0")?, &version("nightly")?),
        Err(UpgradeRefusal::Candidate(
            VersionOrderError::MalformedCore { fields: 1 }
        )),
        "an unorderable candidate is a different operational fact from an unorderable incumbent"
    );
    Ok(())
}

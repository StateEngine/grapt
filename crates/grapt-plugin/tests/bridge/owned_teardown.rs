//! Fixture teardown of exactly one owned containment unit, and the rule
//! that a witness judgment scores only over a proven teardown.

use std::error::Error;
use std::process::Command;
use std::time::Duration;

/// Fixture teardown of exactly one owned unit, independent of the product's
/// stop: it runs on every exit path, and `finish` reports its outcome.
pub(super) struct OwnedUnitTeardown(pub(super) Option<String>);

impl OwnedUnitTeardown {
    pub(super) fn finish(mut self) -> Result<(), Box<dyn Error>> {
        self.0
            .take()
            .map_or(Ok(()), |unit| stop_owned_fixture_unit(&unit))
    }
}

/// The fallback when `finish` was never reached: the test is already
/// failing on another error, so a teardown failure cannot be returned and
/// is named instead, never counted as proven absence.
impl Drop for OwnedUnitTeardown {
    fn drop(&mut self) {
        if let Some(unit) = self.0.take() {
            if let Err(error) = stop_owned_fixture_unit(&unit) {
                eprintln!("fixture teardown fallback failed: {error}");
            }
        }
    }
}

/// Bound for proving the owned fixture unit absent after its teardown.
const FIXTURE_ABSENCE_BOUND: Duration = Duration::from_secs(15);

/// Kills, stops and resets exactly `owned`, then proves it absent. Each
/// systemctl call is bounded; an already absent unit refuses them, so the
/// outcome is judged only by the absence poll, whose every inventory is
/// bounded by what remains of its deadline.
fn stop_owned_fixture_unit(owned: &str) -> Result<(), Box<dyn Error>> {
    for arguments in [
        &["kill", "--kill-whom=all", "--signal=KILL", owned][..],
        &["stop", owned],
        &["reset-failed", owned],
    ] {
        let mut command = Command::new("/usr/bin/systemctl");
        command.arg("--user").args(arguments);
        let _ = super::unit_identity::bounded_output(
            command,
            super::unit_identity::QUERY_BOUND,
            super::unit_identity::OUTPUT_BYTES,
        );
    }
    let deadline = tokio::time::Instant::now() + FIXTURE_ABSENCE_BOUND;
    let mut last = String::from("no inventory completed");
    while let Some(left) = deadline
        .checked_duration_since(tokio::time::Instant::now())
        .filter(|left| !left.is_zero())
    {
        match super::unit_identity::unit_inventory(
            owned,
            left.min(super::unit_identity::QUERY_BOUND),
        ) {
            Ok(units) if units.is_empty() => return Ok(()),
            Ok(units) => last = format!("the inventory still listed {units:?}"),
            Err(error) => last = format!("the last inventory failed: {error}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "fixture teardown left owned unit {owned} standing or unproven after \
         {FIXTURE_ABSENCE_BOUND:?}: {last}"
    )
    .into())
}

/// The production judgment scores only over a proven fixture teardown: a
/// teardown failure voids the witness, so an intended failure never hides
/// an owned unit left standing or unproven.
pub(super) fn judged_after_teardown(
    production: Result<(), Box<dyn Error>>,
    fixture: Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    if let Err(error) = fixture {
        return Err(format!(
            "the fixture teardown was not proven, so the witness is void: {error}"
        )
        .into());
    }
    production
}

#[test]
fn a_fixture_teardown_failure_voids_the_owned_unit_judgment() {
    const INTENDED: &str = "OOM containment unit remained after the attributed failure";
    let failed = || -> Result<(), Box<dyn Error>> {
        Err("fixture teardown left owned unit standing".into())
    };
    for production in [Err(INTENDED.into()), Ok(())] {
        let message = judged_after_teardown(production, failed())
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            message.contains("teardown was not proven") && !message.contains(INTENDED),
            "an intended failure scored over a failed fixture teardown: {message:?}"
        );
    }
    let intended = judged_after_teardown(Err(INTENDED.into()), Ok(()));
    assert_eq!(
        intended.err().map(|error| error.to_string()).as_deref(),
        Some(INTENDED)
    );
    assert!(judged_after_teardown(Ok(()), Ok(())).is_ok());
}

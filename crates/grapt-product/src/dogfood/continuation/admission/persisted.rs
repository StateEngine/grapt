//! A Session's calibration across processes: restored once per process from
//! its durable notice, and written back only when it moved enough to matter.
use super::{Calibration, SessionCalibrations, TargetKey};
use grapt_session::notice::{CalibrationTarget, TokenCalibration};
use grapt_types::{ResolvedModelTarget, SessionId};

/// A ratio that moved by no more than one part in this many is not written
/// again: the estimate it feeds is already within its noise.
const MOVED_PARTS: u128 = 20;

impl TargetKey {
    /// Stable across processes, unlike the catalog issuance [`Self::matches`]
    /// compares; it names the target without carrying its identities.
    fn digest(&self) -> Option<CalibrationTarget> {
        let fields = serde_json::json!([
            self.model.provider().as_str(),
            self.model.model().as_str(),
            self.model.variant().map(|variant| variant.as_str()),
            self.route.as_str(),
            self.account.as_str(),
            self.credential.as_str(),
            self.policy.as_str(),
        ]);
        CalibrationTarget::new(grapt_event::canonical::canonical_hash_hex(&fields))
    }
}

impl Calibration {
    fn durable(&self) -> Option<TokenCalibration> {
        Some(TokenCalibration {
            target: self.target.digest()?,
            confirmed: self.confirmed,
            estimated: self.estimated,
        })
    }
}

/// Whether `next` is a different target than `written`, or its ratio moved by
/// more than one part in [`MOVED_PARTS`].
fn moved(written: &TokenCalibration, next: &TokenCalibration) -> bool {
    if written.target != next.target {
        return true;
    }
    let before = u128::from(written.confirmed) * u128::from(next.estimated);
    let after = u128::from(next.confirmed) * u128::from(written.estimated);
    before.abs_diff(after).saturating_mul(MOVED_PARTS) > before
}

impl SessionCalibrations {
    /// Seed `session` from its durable calibration, once per process and only
    /// while it holds none, when that calibration was measured on `target`.
    /// An unreadable notice starts conservative, as a Session without one does.
    pub(in crate::dogfood) fn restore(
        &self,
        session: &SessionId,
        target: &ResolvedModelTarget,
        read: impl FnOnce() -> Result<Option<TokenCalibration>, String>,
    ) {
        let mut restored = false;
        self.update(session, |held| {
            restored = std::mem::replace(&mut held.restored, true) || held.calibration.is_some();
        });
        if restored {
            return;
        }
        let durable = match read() {
            Ok(durable) => durable,
            Err(reason) => {
                crate::diagnostics::report(format!(
                    "session {session}: token calibration is unreadable; estimating without it: {reason}"
                ));
                return;
            }
        };
        let key = TargetKey::new(target);
        let Some(durable) =
            durable.filter(|durable| key.digest().as_ref() == Some(&durable.target))
        else {
            return;
        };
        let Some(calibration) = Calibration::new(key, durable.confirmed, durable.estimated) else {
            return;
        };
        self.update(session, |held| {
            if held.calibration.is_none() {
                held.calibration = Some(calibration);
                held.written = Some(durable);
            }
        });
    }

    /// Fold the last admitted request's confirmed input into `session`'s
    /// calibration. Admission folds it only when it plans the next request,
    /// so without this an execution's final measured call never calibrates
    /// anything a restart could keep. `confirmed` reads that attempt's input.
    pub(in crate::dogfood) fn fold_last(
        &self,
        session: &SessionId,
        confirmed: impl FnOnce(&grapt_types::PhysicalAttemptId) -> Option<u64>,
    ) {
        let previous = super::super::lock(&self.0)
            .get(session)
            .and_then(|held| held.previous.clone());
        let Some(prior) = previous else {
            return;
        };
        let Some(attempt) = &prior.attempt else {
            return;
        };
        let Some(calibration) = confirmed(attempt)
            .and_then(|input| Calibration::new(prior.target.clone(), input, prior.estimated))
        else {
            return;
        };
        self.update(session, |held| held.calibration = Some(calibration));
    }

    /// Write `session`'s calibration through `write` when it is new, its target
    /// changed, or its ratio moved; nothing is written otherwise.
    pub(in crate::dogfood) fn persist(
        &self,
        session: &SessionId,
        write: impl FnOnce(&TokenCalibration) -> Result<(), String>,
    ) -> Result<(), String> {
        let next = {
            let sessions = super::super::lock(&self.0);
            let Some(held) = sessions.get(session) else {
                return Ok(());
            };
            let Some(next) = held.calibration.as_ref().and_then(Calibration::durable) else {
                return Ok(());
            };
            if held
                .written
                .as_ref()
                .is_some_and(|written| !moved(written, &next))
            {
                return Ok(());
            }
            next
        };
        write(&next)?;
        self.update(session, |held| held.written = Some(next));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calibration(
        target: char,
        confirmed: u64,
        estimated: u64,
    ) -> Result<TokenCalibration, String> {
        Ok(TokenCalibration {
            target: CalibrationTarget::new(target.to_string().repeat(64)).ok_or("target")?,
            confirmed,
            estimated,
        })
    }

    #[test]
    fn only_a_new_target_or_a_ratio_moved_past_five_percent_is_written_again() -> Result<(), String>
    {
        let written = calibration('a', 1_000, 4_000)?;
        assert!(!moved(&written, &calibration('a', 1_040, 4_000)?));
        assert!(!moved(&written, &calibration('a', 2_000, 8_000)?));
        assert!(moved(&written, &calibration('a', 1_060, 4_000)?));
        assert!(moved(&written, &calibration('a', 940, 4_000)?));
        assert!(moved(&written, &calibration('b', 1_000, 4_000)?));
        Ok(())
    }

    #[test]
    fn a_calibration_one_process_wrote_seeds_the_next_only_on_its_target() -> Result<(), String> {
        let target = crate::dogfood::production::tests::projection_target()?;
        let session: SessionId = "ses_calibration_restart"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        let first = SessionCalibrations::default();
        first.update(&session, |held| {
            held.calibration = Calibration::new(TargetKey::new(&target), 1_000, 4_000);
        });
        let mut log = Vec::new();
        for _ in 0..2 {
            first.persist(&session, |durable| {
                log.push(durable.clone());
                Ok(())
            })?;
        }
        assert_eq!(log.len(), 1, "an unchanged calibration is written once");
        let next = SessionCalibrations::default();
        next.restore(&session, &target, || Ok(log.first().cloned()));
        let restored = next.get(&session).0.ok_or("not restored")?;
        assert_eq!((restored.confirmed, restored.estimated), (1_000, 4_000));
        let mut foreign = log.first().cloned().ok_or("written")?;
        foreign.target = CalibrationTarget::new("b".repeat(64)).ok_or("target")?;
        let other = SessionCalibrations::default();
        other.restore(&session, &target, || Ok(Some(foreign)));
        assert!(
            other.get(&session).0.is_none(),
            "another target's ratio is not believed"
        );
        Ok(())
    }

    #[test]
    fn a_single_measured_call_still_leaves_a_restart_seed() -> Result<(), String> {
        let target = crate::dogfood::production::tests::projection_target()?;
        let session: SessionId = "ses_calibration_single"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        let attempt =
            grapt_types::PhysicalAttemptId::new("attempt-calibration-single").ok_or("attempt")?;
        let first = SessionCalibrations::default();
        first.update(&session, |held| {
            held.previous = Some(super::super::Prior {
                attempt: Some(attempt.clone()),
                target: TargetKey::new(&target),
                items: 1,
                digest: [0; 32],
                estimated: 4_000,
                context: None,
            });
        });
        // Admission never planned a second request, so nothing folded it yet.
        let mut log = Vec::new();
        first.persist(&session, |durable| {
            log.push(durable.clone());
            Ok(())
        })?;
        assert!(log.is_empty());
        first.fold_last(&session, |measured| (*measured == attempt).then_some(1_000));
        first.persist(&session, |durable| {
            log.push(durable.clone());
            Ok(())
        })?;
        assert_eq!(log.len(), 1, "the final call's sample was not written");
        let restarted = SessionCalibrations::default();
        restarted.restore(&session, &target, || Ok(log.first().cloned()));
        let restored = restarted.get(&session).0.ok_or("not restored")?;
        assert_eq!((restored.confirmed, restored.estimated), (1_000, 4_000));
        Ok(())
    }
}

//! A service-wide bound on the threads that read-depth carry checks fan out to.
//!
//! The structural check of a carried artifact is a few small file operations,
//! so on a cold page cache it is bound by I/O latency, not CPU. One Session can
//! carry thousands of artifacts; checking them one at a time leaves the device
//! idle between operations. Helpers overlap that latency. They come from ONE
//! budget per service, so concurrent cold reads share the bound instead of
//! multiplying threads: a read that finds no idle helper checks on its own
//! thread, exactly as before.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use grapt_types::ArtifactRef;

use crate::{CarryDigestBudget, CarryObservation, FilesystemArtifactStore};

/// The helper threads a service lends to structural carry checks.
#[derive(Debug)]
pub struct CarryWorkers {
    idle: Mutex<usize>,
}

impl CarryWorkers {
    /// Helpers per service: cold timing on a 4,879-reference carry flattened
    /// at about this depth.
    pub const DEFAULT_HELPERS: usize = 16;

    pub fn new(helpers: usize) -> Self {
        Self {
            idle: Mutex::new(helpers),
        }
    }

    /// Up to `wanted` helpers, released when the lease drops. A poisoned
    /// budget lends none, so the caller still checks everything itself.
    fn lease(&self, wanted: usize) -> Lease<'_> {
        let count = match self.idle.lock() {
            Ok(mut idle) => {
                let count = (*idle).min(wanted);
                *idle -= count;
                count
            }
            Err(_) => 0,
        };
        Lease {
            workers: self,
            count,
        }
    }
}

impl Default for CarryWorkers {
    fn default() -> Self {
        Self::new(Self::DEFAULT_HELPERS)
    }
}

struct Lease<'a> {
    workers: &'a CarryWorkers,
    count: usize,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Ok(mut idle) = self.workers.idle.lock() {
            *idle += self.count;
        }
    }
}

impl FilesystemArtifactStore {
    /// Writer admission's check: references in order, each re-digested under
    /// one budget, stopping at the first that does not stand.
    pub fn first_digested_carry_failure<'r>(
        &self,
        references: &'r [ArtifactRef],
        mut budget: CarryDigestBudget,
    ) -> Option<(&'r ArtifactRef, CarryObservation)> {
        references.iter().find_map(|reference| {
            let observation = self.observe_carried(reference, &mut budget);
            (observation != CarryObservation::Complete).then_some((reference, observation))
        })
    }

    /// The first reference, in the given order, whose structural carry check
    /// fails, with its observation; `None` when every reference stands.
    ///
    /// The answer does not depend on which thread finishes first: every
    /// reference before the reported one was checked and stood. A helper
    /// that dies leaves its references unchecked, and an unchecked reference
    /// is unreadable, never complete.
    pub fn first_structural_carry_failure<'r>(
        &self,
        references: &'r [ArtifactRef],
        workers: &CarryWorkers,
    ) -> Option<(&'r ArtifactRef, CarryObservation)> {
        let next = AtomicUsize::new(0);
        let check = || {
            let mut checked = Vec::new();
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(reference) = references.get(index) else {
                    return checked;
                };
                checked.push((index, self.observe_carried_structure(reference)));
            }
        };
        let lease = workers.lease(references.len().saturating_sub(1));
        let mut observations = vec![None; references.len()];
        std::thread::scope(|scope| {
            let helpers: Vec<_> = (0..lease.count)
                .filter_map(|_| {
                    std::thread::Builder::new()
                        .name("carry-check".into())
                        .spawn_scoped(scope, check)
                        .ok()
                })
                .collect();
            let mut checked = check();
            for helper in helpers {
                if let Ok(part) = helper.join() {
                    checked.extend(part);
                }
            }
            for (index, observation) in checked {
                if let Some(slot) = observations.get_mut(index) {
                    *slot = Some(observation);
                }
            }
        });
        drop(lease);
        references
            .iter()
            .zip(observations)
            .map(|(reference, observation)| {
                (
                    reference,
                    observation.unwrap_or(CarryObservation::Unreadable),
                )
            })
            .find(|(_, observation)| *observation != CarryObservation::Complete)
    }
}

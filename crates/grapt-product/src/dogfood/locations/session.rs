//! A Session's location, discovered once and reused (SC-REL-11).
//!
//! Discovery spawns Git up to five times. Run on every turn, one slow moment
//! on a loaded Windows host outlasted its deadline and parked a queued prompt
//! with nothing scheduled to run it (the eight-hour soak on 21953b9c, at its
//! 118th cycle). A Session's location is resolved when a turn first needs it
//! and again only when its directory, or the repository that directory is in,
//! changes. The repository is found from the filesystem alone, so a nested
//! repository created in the same directory still moves the instruction
//! roots to it, as discovery on every turn did. A discovery that fails is
//! retried a bounded number of times before the turn is reported as not run
//! yet.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use grapt_types::SessionId;

use super::ResolvedLocation;

/// The pause before each retry of a failed discovery: three attempts in all.
const RETRY_PAUSES: [Duration; 2] = [Duration::from_millis(250), Duration::from_secs(1)];
/// Sessions remembered at once. Forgetting one costs only a fresh discovery.
const MAX_SESSIONS: usize = 4_096;

/// The directory, the repository root it was in, and the location found.
type Remembered = (String, Option<PathBuf>, Arc<ResolvedLocation>);

#[derive(Debug, Default)]
pub(crate) struct SessionLocations(Mutex<HashMap<SessionId, Remembered>>);

impl SessionLocations {
    /// `session`'s location in `directory`: the one already resolved there
    /// while the directory is in the same repository, or a fresh discovery.
    pub(crate) async fn resolve(
        &self,
        session: &SessionId,
        directory: &str,
    ) -> Result<Arc<ResolvedLocation>, String> {
        self.resolve_with(session, directory, |directory| async move {
            if discovery_faulted() {
                return Err("location discovery deadline exceeded".to_owned());
            }
            super::resolve_directory(directory).await
        })
        .await
    }

    async fn resolve_with<'a, D, F>(
        &self,
        session: &SessionId,
        directory: &'a str,
        discover: D,
    ) -> Result<Arc<ResolvedLocation>, String>
    where
        D: Fn(&'a str) -> F,
        F: Future<Output = Result<ResolvedLocation, String>>,
    {
        // Unreadable, it is no boundary to compare: discovery says why.
        let boundary = super::repository_root(Path::new(directory))
            .ok()
            .map(|root| root.map(Path::to_path_buf));
        if let (Some((known, root, location)), Some(boundary)) =
            (self.lock().get(session), &boundary)
        {
            if known == directory && root == boundary {
                return Ok(Arc::clone(location));
            }
        }
        let mut discovered = discover(directory).await;
        for pause in RETRY_PAUSES {
            if discovered.is_ok() {
                break;
            }
            tokio::time::sleep(pause).await;
            discovered = discover(directory).await;
        }
        let location = Arc::new(discovered?);
        let Some(boundary) = boundary else {
            return Ok(location);
        };
        let mut remembered = self.lock();
        if remembered.len() >= MAX_SESSIONS && !remembered.contains_key(session) {
            if let Some(evicted) = remembered.keys().next().cloned() {
                remembered.remove(&evicted);
            }
        }
        remembered.insert(
            session.clone(),
            (directory.to_owned(), boundary, Arc::clone(&location)),
        );
        Ok(location)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Remembered>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Testkit: the discovery attempts, counted from 1 across the process, listed
/// in `GRAPT_TESTKIT_LOCATION_DISCOVERY_FAULTS` time out without running.
#[cfg(feature = "loopback-provider-testkit")]
fn discovery_faulted() -> bool {
    static ATTEMPTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let attempt = ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    std::env::var("GRAPT_TESTKIT_LOCATION_DISCOVERY_FAULTS").is_ok_and(|faults| {
        faults
            .split(',')
            .any(|fault| fault.trim().parse() == Ok(attempt))
    })
}

#[cfg(not(feature = "loopback-provider-testkit"))]
fn discovery_faulted() -> bool {
    false
}

#[cfg(test)]
mod tests;

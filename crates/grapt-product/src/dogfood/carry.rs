//! Selected-session carry admission, never a startup sweep.
//!
//! Copyright OpenAI; Apache-2.0. See notices/10-adoptions/session-carry.md.

use std::collections::VecDeque;
use std::sync::Mutex;

use grapt_artifact::{CarryDigestBudget, CarryWorkers};
use grapt_store::{session_content_refs, CarryEnumerationBounds};
use grapt_types::SessionId;

use super::ServerApplication;

const MAX_VERIFIED_SESSIONS: usize = 64;
const MAX_ROWS: u64 = 1_048_576;
const MAX_ITEMS: usize = 65_536;
const MAX_PAYLOAD_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * grapt_runtime::MAX_BLOCK_CAPTURE_BYTES;

/// Successful admission is process-local: new content is committed by this
/// process only after artifact capture. Restart and eviction require rechecking.
/// Failures never enter the cache. No persistent trusted/verified bit is written.
///
/// Two depths share those rules. Writer admission re-digests every carried
/// object; a read route checks only records, objects and lengths, because every
/// byte it serves is digested by the artifact fetch that returns it. A Session
/// admitted at the digest depth satisfies a read without a second pass.
#[derive(Debug, Default)]
pub(super) struct VerifiedSessions {
    digested: Mutex<VecDeque<SessionId>>,
    structural: Mutex<VecDeque<SessionId>>,
    workers: CarryWorkers,
}

/// How far a carry check reaches into the referenced artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    /// Records, objects and lengths: absent, unreadable and corrupt metadata.
    Structure,
    /// The structural checks plus a bounded re-digest of every object's bytes.
    Digest,
}

impl VerifiedSessions {
    fn queue(&self, depth: Depth) -> &Mutex<VecDeque<SessionId>> {
        match depth {
            Depth::Structure => &self.structural,
            Depth::Digest => &self.digested,
        }
    }

    /// Whether a prior pass at this depth or deeper admitted the Session.
    fn admitted(&self, session: &SessionId, depth: Depth) -> Result<bool, String> {
        let deeper: &[Depth] = match depth {
            Depth::Structure => &[Depth::Digest, Depth::Structure],
            Depth::Digest => &[Depth::Digest],
        };
        for depth in deeper {
            let mut verified = self
                .queue(*depth)
                .lock()
                .map_err(|_| "session carry admission lock is poisoned".to_owned())?;
            if let Some(index) = verified.iter().position(|id| id == session) {
                if let Some(id) = verified.remove(index) {
                    verified.push_back(id);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn record(&self, session: &SessionId, depth: Depth) -> Result<(), String> {
        let mut verified = self
            .queue(depth)
            .lock()
            .map_err(|_| "session carry admission lock is poisoned".to_owned())?;
        if let Some(index) = verified.iter().position(|id| id == session) {
            verified.remove(index);
        }
        if verified.len() == MAX_VERIFIED_SESSIONS {
            verified.pop_front();
        }
        verified.push_back(session.clone());
        Ok(())
    }
}

impl ServerApplication {
    /// Writer admission: every carried object is re-digested.
    pub(super) fn verify_session_carry(&self, session: &SessionId) -> Result<(), String> {
        self.check_session_carry(session, Depth::Digest)
    }

    /// Read admission: the structural checks only. A read never digests the
    /// Session's carried bytes; the fetch that serves an object digests it.
    pub(super) fn verify_session_carry_for_read(&self, session: &SessionId) -> Result<(), String> {
        self.check_session_carry(session, Depth::Structure)
    }

    fn check_session_carry(&self, session: &SessionId, depth: Depth) -> Result<(), String> {
        // The cache never holds its lock while waiting for a database or disk:
        // a writer verifying its carry must not stall independent readers.
        if self.verified_carries.admitted(session, depth)? {
            return Ok(());
        }
        let bounds =
            CarryEnumerationBounds::new(MAX_ROWS, MAX_PAYLOAD_BYTES, MAX_TOTAL_BYTES, MAX_ITEMS)
                .ok_or_else(|| "session carry enumeration bound is zero".to_owned())?;
        let references =
            session_content_refs(self.lock_inbox().store().connection(), session, &bounds)
                .map_err(|error| {
                    format!("session {} carry unreadable: {error}", session.as_str())
                })?;
        let artifacts = self.executor.artifacts();
        let failure = match depth {
            Depth::Structure => artifacts
                .first_structural_carry_failure(&references, &self.verified_carries.workers),
            Depth::Digest => artifacts
                .first_digested_carry_failure(&references, CarryDigestBudget::new(MAX_TOTAL_BYTES)),
        };
        if let Some((reference, observation)) = failure {
            return Err(format!(
                "session {} carry refused: artifact {} is {observation}",
                session.as_str(),
                reference.id().as_str(),
            ));
        }
        self.verified_carries.record(session, depth)
    }
}

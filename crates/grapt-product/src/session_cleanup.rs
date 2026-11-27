//! Offline, conservative session-retention cleanup, not physical erasure.
//!
//! Dispatch before product/platform startup. The runner holds the same
//! `root/registration` election authority as product startup for its entire run.
//! Products must retain that authority for their lifetime; independent raw SQLite
//! writers are outside this coordination model. See `docs/session-cleanup.md`.
//!
//! Newest-activity/current-session cleanup rules, modified by Grapt.
//! Copyright 2023-2026 SpaceXAI; licensed under Apache-2.0.
//! Upstream revision and modification details: notices/10-adoptions/cleanup-stale-sessions.md.

mod evidence;
mod receipts;
mod tables;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod worked_tests;

use std::path::{Path, PathBuf};

use grapt_service::{ElectedControllerAuthority, LockAttemptPolicy};
use grapt_store::commit_projection::ProjectionConnection;
use grapt_store::{
    preflight_metadata_read_only, EventLog, EventStore, OwnedOpenAuthority, StoreReadiness,
};
use grapt_types::{ClockAuthority, DeterministicAuthorities, IdAuthority, SessionId, Timestamp};

/// Arguments after the cleanup subcommand (no executable or subcommand name).
#[derive(Debug, Clone)]
pub struct CleanupCommand {
    service: PathBuf,
    cutoff: Timestamp,
    current: SessionId,
    apply: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetentionReason {
    CurrentSession,
    NotOld,
    Deleted,
    Family,
    IncompleteExecution,
    UnresolvedWork,
    UnfamiliarOrWorkEvidence,
    OperationalEvidence,
    UnattributedWork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupDisposition {
    Eligible,
    Retained(RetentionReason),
    Tombstoned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDecision {
    pub session: SessionId,
    pub last_event_ms: i64,
    pub disposition: CleanupDisposition,
}

/// No transcript text, raw payloads, or credentials enter the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    pub applied: bool,
    pub sessions: Vec<SessionDecision>,
}

#[derive(Debug, thiserror::Error)]
pub enum CleanupError {
    #[error("invalid cleanup arguments: {0}")]
    Arguments(String),
    #[error("cleanup refused: {0}")]
    Refused(String),
    #[error("cleanup evidence is unavailable or incomplete: {0}")]
    Evidence(String),
    /// Each tombstone is its own durable commit; a later error cannot undo it.
    #[error("cleanup stopped at {session}: {reason}; earlier tombstones remain committed")]
    Apply {
        session: SessionId,
        tombstoned: Vec<SessionId>,
        reason: String,
    },
}

impl CleanupCommand {
    /// Requires explicit service, cutoff (Unix milliseconds), and current-session
    /// exclusion. Unknown/duplicate options and positional arguments refuse.
    pub fn parse<I, S>(argv: I) -> Result<Self, CleanupError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut service = None;
        let mut cutoff = None;
        let mut current = None;
        let mut apply = false;
        let mut argv = argv.into_iter();
        let bad = |message: &str| CleanupError::Arguments(message.to_owned());
        let mut count = 0;
        while let Some(option) = argv.next() {
            count += 1;
            if count > 4 {
                return Err(bad("too many or duplicate options"));
            }
            let option = option.as_ref();
            if option == "--apply" {
                if apply {
                    return Err(bad("duplicate --apply"));
                }
                apply = true;
                continue;
            }
            if !matches!(
                option,
                "--service" | "--cutoff-ms" | "--cutoff" | "--current-session"
            ) {
                return Err(bad("unknown option or positional argument"));
            }
            let value = argv.next().ok_or_else(|| bad("option requires a value"))?;
            let value = value.as_ref();
            if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
                return Err(bad("empty, oversized, or control-bearing value"));
            }
            match option {
                "--service" if service.is_none() => {
                    let path = PathBuf::from(value);
                    if !path.is_absolute() {
                        return Err(bad("--service must be absolute"));
                    }
                    service = Some(path);
                }
                "--cutoff-ms" | "--cutoff" if cutoff.is_none() => {
                    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(bad("cutoff must be nonnegative Unix milliseconds"));
                    }
                    cutoff =
                        Some(Timestamp::from_millis(value.parse().map_err(|_| {
                            bad("cutoff exceeds the supported millisecond range")
                        })?));
                }
                "--current-session" if current.is_none() => {
                    current = Some(
                        value
                            .parse()
                            .map_err(|_| bad("invalid current SessionId"))?,
                    );
                }
                _ => return Err(bad("duplicate option")),
            }
        }
        Ok(Self {
            service: service.ok_or_else(|| bad("--service is required"))?,
            cutoff: cutoff.ok_or_else(|| bad("--cutoff-ms is required"))?,
            current: current.ok_or_else(|| bad("--current-session is required"))?,
            apply,
        })
    }

    pub fn service(&self) -> &Path {
        &self.service
    }

    pub fn cutoff(&self) -> Timestamp {
        self.cutoff
    }

    pub fn current_session(&self) -> &SessionId {
        &self.current
    }

    pub fn applies(&self) -> bool {
        self.apply
    }

    /// Diagnostic preview on a caller-owned connection, with no writes or locks.
    /// This is NOT deletion authority or a live-runtime inactivity check. `run`
    /// always takes its own locked, read-only snapshot before applying anything.
    pub fn preview(
        &self,
        connection: &ProjectionConnection,
    ) -> Result<CleanupReport, CleanupError> {
        evidence::scan(&evidence::Source::connection(connection), self)
            .map_err(CleanupError::Evidence)
    }

    /// Run synchronously before any normal startup side effects. `engine_version`
    /// is the install's declared build identity, not the crate version. Dry-run
    /// never opens a writable DB. Apply uses only existing-store OpenReady and
    /// the session owner's tombstone API; it never initializes or migrates.
    pub fn run(
        &self,
        engine_version: &str,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
    ) -> Result<CleanupReport, CleanupError> {
        if self.cutoff.as_millis() >= clock.now().as_millis() {
            return Err(CleanupError::Refused(
                "cutoff must be strictly in the past".into(),
            ));
        }
        let root = self.service.canonicalize().map_err(refused)?;
        let database = root.join("database.sqlite3");
        if !root.is_dir()
            || !std::fs::symlink_metadata(&database)
                .map_err(refused)?
                .is_file()
        {
            return Err(CleanupError::Refused(
                "service must contain a regular existing database".into(),
            ));
        }
        let policy = LockAttemptPolicy::new(3)
            .ok_or_else(|| CleanupError::Refused("invalid lock budget".into()))?;
        let _offline = ElectedControllerAuthority::acquire(root.join("registration"), policy)
            .map_err(refused)?;
        // Cleanup validates every record before its first deletion, so the
        // ordinary open question is what it needs; a full page walk here would
        // only delay the operator, and the walk would not add a check the
        // evidence pass does not already make.
        match preflight_metadata_read_only(&database) {
            StoreReadiness::Ready => {}
            other => {
                return Err(CleanupError::Refused(format!(
                    "existing ready store required: {other:?}"
                )))
            }
        }
        let mut report = {
            let log = EventLog::open_read_only(&database).map_err(refused)?;
            log.with_read_snapshot(|snapshot| {
                evidence::scan(&evidence::Source::snapshot(snapshot), self)
            })
            .map_err(CleanupError::Evidence)?
        };
        if !self.apply
            || !report
                .sessions
                .iter()
                .any(|row| row.disposition == CleanupDisposition::Eligible)
        {
            return Ok(report);
        }
        if engine_version.trim().is_empty() {
            return Err(CleanupError::Refused(
                "declared build identity is required for apply".into(),
            ));
        }
        let mut owned = EventStore::open_owned_with_authorities(
            &database,
            engine_version,
            OwnedOpenAuthority::OpenReady,
            clock,
            ids,
        )
        .map_err(refused)?;
        // Owned-open may install auxiliary schemas. Inspect again, without
        // releasing the product exclusion, and validate every record before the
        // first deletion so corrupt later candidates cannot cause a partial sweep.
        report = self.preview(owned.store().connection())?;
        for row in &report.sessions {
            if row.disposition == CleanupDisposition::Eligible {
                grapt_session::SessionStore::over(owned.store())
                    .record(&row.session)
                    .map_err(|error| CleanupError::Evidence(error.to_string()))?;
            }
        }
        let authorities = DeterministicAuthorities::new(clock, ids);
        let mut tombstoned = Vec::new();
        for row in &mut report.sessions {
            if row.disposition != CleanupDisposition::Eligible {
                continue;
            }
            if let Err(error) =
                grapt_session::delete_session(owned.store_mut(), &authorities, &row.session)
            {
                return Err(CleanupError::Apply {
                    session: row.session.clone(),
                    tombstoned,
                    reason: error.to_string(),
                });
            }
            tombstoned.push(row.session.clone());
            row.disposition = CleanupDisposition::Tombstoned;
        }
        report.applied = true;
        Ok(report)
    }
}

fn refused(error: impl std::fmt::Display) -> CleanupError {
    CleanupError::Refused(error.to_string())
}

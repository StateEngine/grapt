//! The one gate every macOS kill by PID or group ID passes (SC-MAC-02).
//!
//! Without a kernel container, a kill names a number, and a number can be
//! someone else's. So no target is signalled unless it is provably the
//! bridge's: never 0 (the caller's own group), 1 (launchd), an ID that wraps
//! negative, or the supervisor's own process or group. A process is signalled
//! only while its recorded identity (PID plus start time) still matches. A
//! group is signalled only on the authority of an unreaped child of ours that
//! is in that group right now: an unreaped child's PID cannot be reused, and a
//! group ID is never reused while one of its members is unreaped, so the ID
//! is still the bridge's even after the leader was reaped. Anything that
//! cannot be verified is refused and reported, never guessed.
//!
//! A single process outside the group is signalled only if, re-read just
//! before the signal, its start time equals the recorded one and is no
//! earlier than the leader's, and its parent is the recorded parent with that
//! parent's own identity unchanged, or launchd for an escapee it adopted.
//! Residual risk, stated with `ContainmentKind::Watchdog`: start times are
//! whole seconds and macOS has no process handle to signal through, so a
//! process that exits and whose PID is reused, by a child of the same
//! recorded parent, within the same second and between the re-read and the
//! signal, would be signalled.
//!
//! Killing an unreaped `std::process::Child` through its own handle needs no
//! gate: the kernel holds that identity until the reap.

use std::fmt;

/// A recorded process identity: the process, and the parent it was seen under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::supervisor) struct Identity {
    pub(super) pid: u32,
    pub(super) started: u64,
    pub(super) parent: Option<u32>,
    pub(super) parent_started: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// One process, signalled only while its identity still matches and it
    /// started no earlier than `floor`, the bridge leader's start.
    Process { identity: Identity, floor: u64 },
    /// A whole group, authorized by `holder`: an unreaped child of ours whose
    /// current group must be `group`. The caller holds that child unreaped.
    Group { group: u32, holder: u32 },
    /// A crashed supervisor's sentinel group, authorized only by a verified
    /// journal record (`journal::RecordedGroup`, constructible nowhere else).
    RecordedGroup(super::journal::RecordedGroup),
}

/// The supervisor itself, which is never a target.
#[derive(Clone, Copy, Debug)]
pub(super) struct Own {
    pub(super) pid: u32,
    pub(super) group: u32,
}

/// A process as the platform reads it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Seen {
    Alive {
        started: u64,
        parent: Option<u32>,
    },
    /// No such process: there is nothing to kill.
    Gone,
    /// The process exists but its identity could not be read.
    Unreadable,
}

/// What one kernel lookup by PID answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lookup {
    Found,
    /// ESRCH: the kernel has no live process by that PID.
    Missing,
    /// Any other failure: nothing is known.
    Failed,
}

/// A PID the process table could not describe, judged by two kernel
/// lookups: a null signal, which on Darwin still finds an unreaped zombie,
/// and the group lookup, which does not. Missing from the first, there is no
/// process; found by the first and missing from the second, it is a zombie,
/// dead and unable to run, so there is nothing to kill either. Anything else
/// is unreadable and refused.
pub(super) fn unlisted(null_signal: Lookup, group: Lookup) -> Seen {
    match (null_signal, group) {
        (Lookup::Missing, _) | (Lookup::Found, Lookup::Missing) => Seen::Gone,
        _ => Seen::Unreadable,
    }
}

pub(super) trait Probe {
    fn observe(&self, pid: u32) -> Seen;
    fn group_of(&self, pid: u32) -> Option<u32>;
}

/// launchd adopts every orphan on macOS.
const LAUNCHD: u32 = 1;

/// The kills themselves. Production maps "no such process" to `Ok`.
pub(super) trait Signals {
    fn kill(&mut self, pid: i32) -> Result<(), String>;
    fn kill_group(&mut self, group: i32) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sent {
    Signalled,
    /// The process had already exited.
    Gone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// 0, 1 or a wrapping ID: no bridge process can hold it.
    NotABridgeId(u32),
    /// The supervisor's own process or group.
    Own(u32),
    /// The PID now belongs to a different process: not the bridge's.
    Reused(u32),
    /// The process started before the bridge leader did.
    BeforeLeader(u32),
    /// The process's parent, or that parent's identity, is not the recorded one.
    ParentChanged(u32),
    /// The target could not be verified as the bridge's.
    Unverified(u32),
    /// The kill itself failed.
    Failed(u32, String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotABridgeId(id) => write!(formatter, "{id} is not a bridge process ID"),
            Self::Own(id) => write!(formatter, "{id} is the supervisor's own process or group"),
            Self::Reused(pid) => write!(formatter, "PID {pid} now belongs to another process"),
            Self::BeforeLeader(pid) => {
                write!(formatter, "PID {pid} started before the bridge leader")
            }
            Self::ParentChanged(pid) => {
                write!(
                    formatter,
                    "PID {pid} is no longer under its recorded parent"
                )
            }
            Self::Unverified(id) => write!(formatter, "{id} could not be verified as the bridge's"),
            Self::Failed(id, error) => write!(formatter, "{id} could not be killed: {error}"),
        }
    }
}

/// The checked form of an ID a bridge process or group can hold.
pub(super) fn bridge_id(raw: u32, own: Own) -> Result<i32, Refusal> {
    let id = i32::try_from(raw)
        .ok()
        .filter(|id| *id > 1)
        .ok_or(Refusal::NotABridgeId(raw))?;
    if raw == own.pid || raw == own.group {
        return Err(Refusal::Own(raw));
    }
    Ok(id)
}

/// Send SIGKILL to `target` only if it is provably the bridge's.
pub(super) fn signal(
    target: Target,
    own: Own,
    probe: &impl Probe,
    signals: &mut impl Signals,
) -> Result<Sent, Refusal> {
    match target {
        Target::Process { identity, floor } => {
            let pid = bridge_id(identity.pid, own)?;
            // Re-read now: the identity a sample recorded may be stale.
            let (started, parent) = match probe.observe(identity.pid) {
                Seen::Gone => return Ok(Sent::Gone),
                Seen::Unreadable => return Err(Refusal::Unverified(identity.pid)),
                Seen::Alive { started, parent } => (started, parent),
            };
            if started != identity.started {
                return Err(Refusal::Reused(identity.pid));
            }
            if started < floor {
                return Err(Refusal::BeforeLeader(identity.pid));
            }
            if parent != Some(LAUNCHD) && !parent_holds(identity, parent, probe) {
                return Err(Refusal::ParentChanged(identity.pid));
            }
            signals
                .kill(pid)
                .map(|()| Sent::Signalled)
                .map_err(|error| Refusal::Failed(identity.pid, error))
        }
        Target::RecordedGroup(target) => {
            let id = bridge_id(target.group(), own)?;
            bridge_id(target.sentinel().pid, own)?;
            if !super::journal::holds(&target, probe) {
                return Err(Refusal::Unverified(target.group()));
            }
            signals
                .kill_group(id)
                .map(|()| Sent::Signalled)
                .map_err(|error| Refusal::Failed(target.group(), error))
        }
        Target::Group { group, holder } => {
            let id = bridge_id(group, own)?;
            bridge_id(holder, own)?;
            if probe.group_of(holder) != Some(group) {
                return Err(Refusal::Unverified(group));
            }
            signals
                .kill_group(id)
                .map(|()| Sent::Signalled)
                .map_err(|error| Refusal::Failed(group, error))
        }
    }
}

/// `signal`, re-reading a process that cannot be read until `deadline`.
/// Darwin cannot describe a killed process that is still unreaped (a
/// zombie), though a null signal finds it, so right after a group kill its
/// members read as unreadable until their parent or launchd reaps them; then
/// they read as gone. One still unreadable at the deadline is refused as
/// before. A group's refusal is never retried.
pub(super) fn signal_settled(
    target: Target,
    own: Own,
    probe: &impl Probe,
    signals: &mut impl Signals,
    deadline: tokio::time::Instant,
) -> Result<Sent, Refusal> {
    loop {
        match signal(target, own, probe, signals) {
            Err(Refusal::Unverified(_))
                if matches!(target, Target::Process { .. })
                    && tokio::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            outcome => return outcome,
        }
    }
}

/// Whether `parent` is the recorded parent and that parent is still the
/// process it was when recorded.
fn parent_holds(identity: Identity, parent: Option<u32>, probe: &impl Probe) -> bool {
    let (Some(recorded), Some(recorded_started)) = (identity.parent, identity.parent_started)
    else {
        return false;
    };
    parent == Some(recorded)
        && matches!(
            probe.observe(recorded),
            Seen::Alive { started, .. } if started == recorded_started
        )
}

#[cfg(test)]
#[path = "signal_tests.rs"]
mod tests;

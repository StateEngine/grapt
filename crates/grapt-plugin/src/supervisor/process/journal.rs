//! The bridge launch journal and its reclaim after a supervisor crash
//! (SC-MAC-02b), kept OS-light so every decision is proven with fakes.
//!
//! macOS gives a dead supervisor's bridge trees to launchd, and nothing
//! reaps them. So every launch is journalled before anything is spawned:
//! an intent record (a nonce, the owning supervisor's identity and the
//! intent time) is published durably first; the sentinel is spawned as a
//! new group leader carrying the nonce in its argv[0]; its identity is
//! published; only then is the bridge spawned into the sentinel's group. No
//! bridge can exist without a published record naming its group's sentinel.
//!
//! A successor reclaims a record only when every check holds, re-read just
//! before the one group signal it may send (`RecordedGroup`): the recorded
//! sentinel still leads its group, under its recorded start time, adopted by
//! launchd or still under its owner, and the owner is gone. An intent-only
//! record (a crash before the sentinel was published) may name its sentinel
//! only by the nonce in argv[0], from exactly one live process of our user
//! whose parent and start time fit. A record that cannot be verified is
//! preserved and reported, never signalled; only a record proven stale (no
//! sentinel and no group left) is deleted. Residual risk: a sentinel killed
//! by someone else leaves its group unpinned; its members are reported, not
//! signalled.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::signal::{self, Identity, Own, Probe, Seen, Signals, Target};
use super::PluginError;

pub(super) const RECORD_VERSION: u32 = 1;
pub(super) const MAX_RECORD_BYTES: u64 = 4096;
const LOCK_FILE: &str = "reclaim.lock";
const RECORD_SUFFIX: &str = ".json";
/// argv[0] of every sentinel; the nonce follows it.
pub(super) const SENTINEL_TAG: &str = "secode-plugin-sentinel-";
const LAUNCHD: u32 = 1;

/// A recorded process: its PID and start time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Recorded {
    pub(super) pid: u32,
    pub(super) started: u64,
}

/// One bridge launch, as journalled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub(super) version: u32,
    pub(super) nonce: String,
    pub(super) uid: u32,
    pub(super) owner: Recorded,
    /// The owning supervisor's clock at intent, in milliseconds.
    pub(super) intent_millis: i64,
    /// Published once the sentinel exists; absent in an intent-only record.
    pub(super) sentinel: Option<Recorded>,
}

impl Record {
    /// The sentinel's argv[0].
    pub(super) fn tag(&self) -> String {
        format!("{SENTINEL_TAG}{}", self.nonce)
    }
}

/// A nonce is used as a file name and in argv[0]: it must be short and plain.
pub(super) fn valid_nonce(nonce: &str) -> bool {
    (8..=64).contains(&nonce.len())
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// The authority to signal one recorded sentinel's group after a crash.
/// Its fields are private and its only constructor is `decide`, after the
/// record verified, so no other code path can build one; the gate re-reads
/// every condition before the one `killpg` it allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordedGroup {
    group: u32,
    sentinel: Identity,
    owner: Recorded,
}

impl RecordedGroup {
    pub(super) fn group(&self) -> u32 {
        self.group
    }

    pub(super) fn sentinel(&self) -> Identity {
        self.sentinel
    }
}

/// What the process table can say beyond one process.
pub(super) trait Census: Probe {
    /// The live members of `group`, or `None` if they cannot be listed.
    fn members(&self, group: u32) -> Option<Vec<u32>>;
    /// Live processes of `uid` whose argv[0] is `tag`, or `None` if some
    /// process of that user could not be read.
    fn tagged(&self, tag: &str, uid: u32) -> Option<Vec<u32>>;
    /// Whether `pid` may still be an exited, unreaped process. A table that
    /// lists zombies already reads one as alive; Darwin's does not, so its
    /// census asks the kernel. A launch is not ended until its sentinel is
    /// reaped.
    fn unreaped(&self, _pid: u32) -> bool {
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Decision {
    /// The owning supervisor still runs: its record is not an orphan.
    LiveOwner,
    /// Nothing of the launch remains: the record may be deleted.
    Stale,
    /// The record cannot be verified: preserved and reported.
    Unverified(String),
    Reclaim(RecordedGroup),
}

/// Decide one record. Nothing here signals.
pub(super) fn decide(record: &Record, census: &impl Census, own: Own) -> Decision {
    let unverified = |why: &str| Decision::Unverified(format!("{}: {why}", record.nonce));
    if record.version != RECORD_VERSION || !valid_nonce(&record.nonce) || record.intent_millis < 0 {
        return unverified("unsupported record");
    }
    match census.observe(record.owner.pid) {
        Seen::Alive { started, .. } if started == record.owner.started => {
            return Decision::LiveOwner;
        }
        Seen::Unreadable => return unverified("its owner cannot be read"),
        Seen::Gone | Seen::Alive { .. } => {}
    }
    let sentinel = match record.sentinel {
        Some(sentinel) => sentinel,
        None => {
            // Intent only: the nonce in argv[0] is the only way to name it.
            let Some(tagged) = census.tagged(&record.tag(), record.uid) else {
                return unverified("a process of its user cannot be read");
            };
            // Any candidate that cannot be read leaves discovery uncertain.
            let mut fitting = Vec::new();
            for pid in tagged {
                match census.observe(pid) {
                    Seen::Unreadable => {
                        return unverified("a process carrying its nonce cannot be read")
                    }
                    Seen::Alive { started, parent }
                        if fits_parent(parent, record.owner.pid)
                            && started_by(started, record.intent_millis) =>
                    {
                        fitting.push(Recorded { pid, started });
                    }
                    Seen::Gone | Seen::Alive { .. } => {}
                }
            }
            match fitting.as_slice() {
                [] => return stale_unless_members(record, None, census),
                [only] => *only,
                _ => return unverified("more than one process carries its nonce"),
            }
        }
    };
    match census.observe(sentinel.pid) {
        Seen::Gone if census.unreaped(sentinel.pid) => {
            unverified("its sentinel has exited but is not yet reaped")
        }
        Seen::Gone => stale_unless_members(record, Some(sentinel.pid), census),
        Seen::Unreadable => unverified("its sentinel cannot be read"),
        Seen::Alive { started, .. } if started != sentinel.started => {
            unverified("its sentinel's PID now runs another process")
        }
        Seen::Alive { parent, .. } => {
            if !fits_parent(parent, record.owner.pid) {
                return unverified("its sentinel has an unexpected parent");
            }
            if census.group_of(sentinel.pid) != Some(sentinel.pid) {
                return unverified("its sentinel no longer leads its group");
            }
            if signal::bridge_id(sentinel.pid, own).is_err() {
                return unverified("its sentinel is not a bridge process ID");
            }
            Decision::Reclaim(RecordedGroup {
                group: sentinel.pid,
                sentinel: Identity {
                    pid: sentinel.pid,
                    started: sentinel.started,
                    parent: parent.filter(|parent| *parent != LAUNCHD),
                    parent_started: None,
                },
                owner: record.owner,
            })
        }
    }
}

/// Whether a start in whole seconds is at or after an intent in milliseconds,
/// allowing the second the intent fell in. Arithmetic is checked: an
/// out-of-range value never fits.
fn started_by(started: u64, intent_millis: i64) -> bool {
    let Some(started_millis) = i64::try_from(started)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1000))
        .and_then(|millis| millis.checked_add(999))
    else {
        return false;
    };
    started_millis >= intent_millis
}

fn fits_parent(parent: Option<u32>, owner: u32) -> bool {
    parent == Some(LAUNCHD) || parent == Some(owner)
}

/// A gone sentinel is stale only if no member of its group is left: those
/// are unpinned now, and are reported rather than signalled.
fn stale_unless_members(record: &Record, group: Option<u32>, census: &impl Census) -> Decision {
    let Some(group) = group else {
        return Decision::Stale;
    };
    match census.members(group) {
        Some(members) if members.is_empty() => Decision::Stale,
        Some(members) => Decision::Unverified(format!(
            "{}: its sentinel is gone and {} unpinned member(s) remain",
            record.nonce,
            members.len()
        )),
        None => Decision::Unverified(format!(
            "{}: its group's members cannot be listed",
            record.nonce
        )),
    }
}

/// The gate's check for a reclaim target, re-read just before the signal:
/// the owner is still gone (absent, or its PID reused), and the sentinel
/// still leads the group under its recorded identity and an expected parent.
pub(super) fn holds(target: &RecordedGroup, census: &impl Probe) -> bool {
    let sentinel = target.sentinel;
    let owner_gone = match census.observe(target.owner.pid) {
        Seen::Gone => true,
        Seen::Alive { started, .. } => started != target.owner.started,
        Seen::Unreadable => false,
    };
    owner_gone
        && sentinel.pid == target.group
        && matches!(
            census.observe(sentinel.pid),
            Seen::Alive { started, parent }
                if started == sentinel.started && fits_parent(parent, target.owner.pid)
        )
        && census.group_of(sentinel.pid) == Some(target.group)
}

/// The journal directory: ours alone, or refused.
#[derive(Debug)]
pub(super) struct Journal {
    dir: PathBuf,
    uid: u32,
    #[cfg(test)]
    fault: Mutex<Option<Fault>>,
}

/// The durable steps of a publication, for injected-failure witnesses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    Write,
    Sync,
    Rename,
    DirSync,
}

/// Holding the journal's permanent lock: only one reclaim at a time. The
/// lock is released when the file closes.
#[derive(Debug)]
pub(super) struct Claim {
    _locked: File,
}

fn io(context: &str, error: std::io::Error) -> PluginError {
    PluginError::Runtime(format!("plugin recovery journal {context}: {error}"))
}

impl Journal {
    /// Open, creating it with mode 0700 if absent. An existing path must be
    /// a real directory of `uid` with mode 0700, or the journal is refused.
    pub(super) fn open(dir: &Path, uid: u32) -> Result<Self, PluginError> {
        Self::open_with(dir, uid, false)
    }

    /// `open`, with a test-injected failure of the parent's sync.
    pub(super) fn open_with(
        dir: &Path,
        uid: u32,
        fail_parent_sync: bool,
    ) -> Result<Self, PluginError> {
        match DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io("could not be created", error)),
        }
        // The directory is durable only once its parent is synced, and no
        // launch may be journalled until it is. Every open syncs, so a sync
        // that once failed is never skipped by a retry.
        let synced = if fail_parent_sync {
            Err(std::io::Error::other("injected parent sync failure"))
        } else {
            dir.parent().map_or(Ok(()), |parent| {
                File::open(parent).and_then(|parent| parent.sync_all())
            })
        };
        synced.map_err(|error| io("parent could not be synced", error))?;
        let metadata =
            std::fs::symlink_metadata(dir).map_err(|error| io("is unreadable", error))?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
            return Err(PluginError::Runtime(format!(
                "plugin recovery journal {} is not a private directory of this user",
                dir.display()
            )));
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            uid,
            #[cfg(test)]
            fault: Mutex::new(None),
        })
    }

    /// A test-injected failure at one durable step of `publish`.
    #[cfg(test)]
    fn fault(&self, step: Fault) -> std::io::Result<()> {
        match self.fault.lock().map(|fault| *fault) {
            Ok(Some(injected)) if injected == step => {
                Err(std::io::Error::other(format!("injected {step:?} failure")))
            }
            _ => Ok(()),
        }
    }

    #[cfg(not(test))]
    fn fault(&self, _step: Fault) -> std::io::Result<()> {
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn inject(&self, step: Option<Fault>) {
        if let Ok(mut fault) = self.fault.lock() {
            *fault = step;
        }
    }

    fn sync_dir(&self) -> Result<(), PluginError> {
        self.fault(Fault::DirSync)
            .and_then(|()| File::open(&self.dir))
            .and_then(|dir| dir.sync_all())
            .map_err(|error| io("could not be synced", error))
    }

    /// Publish a record durably: a fresh temporary file, synced, renamed
    /// over the record, then the directory synced.
    pub(super) fn publish(&self, record: &Record) -> Result<(), PluginError> {
        if !valid_nonce(&record.nonce) {
            return Err(PluginError::Runtime(
                "plugin recovery nonce is invalid".to_owned(),
            ));
        }
        let bytes = serde_json::to_vec(record)
            .map_err(|error| PluginError::Runtime(format!("plugin recovery record: {error}")))?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(PluginError::Runtime(
                "plugin recovery record is oversized".to_owned(),
            ));
        }
        let temporary = self.dir.join(format!(".{}.tmp", record.nonce));
        self.discard_unpublished(&temporary);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| io("record could not be created", error))?;
        let written = self
            .fault(Fault::Write)
            .and_then(|()| file.write_all(&bytes))
            .and_then(|()| self.fault(Fault::Sync))
            .and_then(|()| file.sync_all())
            .and_then(|()| self.fault(Fault::Rename))
            .and_then(|()| {
                std::fs::rename(
                    &temporary,
                    self.dir.join(format!("{}{RECORD_SUFFIX}", record.nonce)),
                )
            });
        if let Err(error) = written {
            self.discard_unpublished(&temporary);
            return Err(io("record could not be published", error));
        }
        self.sync_dir()
    }

    /// Delete an unpublished temporary file. It is never a record: the scan
    /// reads only `*.json`, and a temporary is renamed into one only by
    /// `publish`.
    fn discard_unpublished(&self, temporary: &Path) {
        let _ = std::fs::remove_file(temporary);
    }

    /// Retire a record: the only deletion of a journal record, and only on a
    /// fresh census proof that its launch ended. Every other outcome keeps it.
    pub(super) fn retire_record(&self, proof: Ended) -> Result<(), PluginError> {
        match std::fs::remove_file(self.dir.join(format!("{}{RECORD_SUFFIX}", proof.nonce))) {
            Ok(()) => self.sync_dir(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io("record could not be retired", error)),
        }
    }

    /// Take the permanent lock, or `None` if another reclaim holds it. The
    /// lock file is never replaced or removed, so every claimant locks the
    /// same inode.
    pub(super) fn claim(&self) -> Result<Option<Claim>, PluginError> {
        let path = self.dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|error| io("lock could not be opened", error))?;
        let metadata = file
            .metadata()
            .map_err(|error| io("lock is unreadable", error))?;
        if !metadata.is_file() || metadata.uid() != self.uid {
            return Err(PluginError::Runtime(
                "plugin recovery lock is not this user's file".to_owned(),
            ));
        }
        match file.try_lock() {
            Ok(()) => Ok(Some(Claim { _locked: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(io("lock failed", error)),
        }
    }

    /// Read at most `limit` records, each bounded. A record that is not a
    /// private regular file of this user, oversized or unparseable, is
    /// reported unverified and left in place.
    pub(super) fn scan(&self, limit: usize) -> Result<Vec<Scanned>, PluginError> {
        let mut scanned = Vec::new();
        for entry in std::fs::read_dir(&self.dir).map_err(|error| io("is unreadable", error))? {
            let entry = entry.map_err(|error| io("is unreadable", error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(nonce) = name.strip_suffix(RECORD_SUFFIX) else {
                continue;
            };
            if scanned.len() >= limit {
                break;
            }
            scanned.push(self.read(&entry.path(), nonce));
        }
        Ok(scanned)
    }

    fn read(&self, path: &Path, nonce: &str) -> Scanned {
        let refuse = |why: String| Scanned::Unverified(format!("{nonce}: {why}"));
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => return refuse(error.to_string()),
        };
        if !metadata.is_file()
            || metadata.uid() != self.uid
            || metadata.mode() & 0o777 != 0o600
            || metadata.len() > MAX_RECORD_BYTES
        {
            return refuse("not a private record of this user".to_owned());
        }
        let mut text = String::new();
        let read = File::open(path).and_then(|file| {
            std::io::Read::read_to_string(
                &mut std::io::Read::take(file, MAX_RECORD_BYTES),
                &mut text,
            )
        });
        if let Err(error) = read {
            return refuse(error.to_string());
        }
        match serde_json::from_str::<Record>(&text) {
            Ok(record) if record.nonce == nonce => Scanned::Record(record),
            Ok(_) => refuse("its nonce does not match its name".to_owned()),
            Err(error) => refuse(error.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Scanned {
    Record(Record),
    Unverified(String),
}

/// One process's group as read for a census.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GroupRead {
    In(u32),
    /// The process exited while being read.
    Gone,
    /// Its group could not be read: the census is incomplete.
    Unreadable,
}

/// The members of `group`, or `None` when any process's group could not be
/// read: an incomplete census is "unknown", never an empty group.
pub(super) fn census_members(
    group: u32,
    reads: impl IntoIterator<Item = (u32, GroupRead)>,
) -> Option<Vec<u32>> {
    let mut members = Vec::new();
    for (pid, read) in reads {
        match read {
            GroupRead::In(found) if found == group => members.push(pid),
            GroupRead::In(_) | GroupRead::Gone => {}
            GroupRead::Unreadable => return None,
        }
    }
    Some(members)
}

/// A fresh census proof that nothing of a launch is left: the only
/// authority that may retire its record. Its field is private and its only
/// constructor is `prove_ended`, so no other path can delete a record.
#[derive(Debug)]
pub(super) struct Ended {
    nonce: String,
}

/// Prove, from one fresh census, that a launch ended: an intent-only record
/// has no live process of its user carrying its nonce; a launched one's
/// sentinel is gone (or its PID now runs someone else) and its group has no
/// member. Anything unknown is no proof.
pub(super) fn prove_ended(record: &Record, census: &impl Census) -> Option<Ended> {
    let ended = match record.sentinel {
        None => census.tagged(&record.tag(), record.uid)?.is_empty(),
        Some(sentinel) => {
            let gone = match census.observe(sentinel.pid) {
                Seen::Gone => !census.unreaped(sentinel.pid),
                Seen::Alive { started, .. } => started != sentinel.started,
                Seen::Unreadable => false,
            };
            gone && census.members(sentinel.pid)?.is_empty()
        }
    };
    ended.then(|| Ended {
        nonce: record.nonce.clone(),
    })
}

/// Retire the record if the census proves its launch ended; otherwise keep
/// it and say why.
fn retire_if_ended(
    journal: &Journal,
    record: &Record,
    census: &impl Census,
) -> Result<(), PluginError> {
    match prove_ended(record, census) {
        Some(proof) => journal.retire_record(proof),
        None => Err(PluginError::Runtime(format!(
            "plugin launch {} was not proven ended; its record is kept",
            record.nonce
        ))),
    }
}

/// What a launch spawns, behind a seam so every crash cut is witnessed.
pub(super) trait Launcher {
    type Sentinel;
    type Bridge;
    /// Spawn the sentinel as a new group leader whose argv[0] is `tag`.
    fn sentinel(&mut self, tag: &str) -> Result<(Self::Sentinel, Recorded), NotLaunched>;
    /// Spawn the bridge into the sentinel's `group`.
    fn bridge(&mut self, group: u32) -> Result<Self::Bridge, PluginError>;
    /// Kill and reap a sentinel whose launch failed, within its bound.
    fn abandon(&mut self, sentinel: Self::Sentinel) -> Result<(), PluginError>;
}

/// A sentinel step that failed: whether it is confirmed that nothing it
/// spawned is left. Only then may the intent be forgotten; otherwise the
/// intent stays, so the survivor is discoverable by its nonce.
#[derive(Debug)]
pub(super) struct NotLaunched {
    pub(super) error: PluginError,
    pub(super) nothing_left: bool,
}

/// Journal, then spawn: the intent is durable before the sentinel exists,
/// and the sentinel's identity before the bridge does. A failure after the
/// sentinel abandons it within its bound; its record is removed only once
/// that is confirmed, so an unconfirmed sentinel stays reclaimable.
pub(super) fn launch<L: Launcher>(
    journal: &Journal,
    launcher: &mut L,
    census: &impl Census,
    intent: Record,
) -> Result<(L::Sentinel, L::Bridge, Record), PluginError> {
    journal.publish(&intent)?;
    let (sentinel, recorded) = match launcher.sentinel(&intent.tag()) {
        Ok(spawned) => spawned,
        Err(NotLaunched {
            error,
            nothing_left: true,
        }) => {
            return Err(match retire_if_ended(journal, &intent, census) {
                Ok(()) => error,
                Err(kept) => PluginError::Spawn(format!("{error}; {kept}")),
            });
        }
        Err(NotLaunched { error, .. }) => {
            return Err(PluginError::Spawn(format!(
                "{error}; a sentinel may survive, so its intent is kept"
            )))
        }
    };
    let record = Record {
        sentinel: Some(recorded),
        ..intent
    };
    if let Err(error) = journal.publish(&record) {
        return Err(abandoned(
            journal, launcher, census, sentinel, &record, error,
        ));
    }
    match launcher.bridge(recorded.pid) {
        Ok(bridge) => Ok((sentinel, bridge, record)),
        Err(error) => Err(abandoned(
            journal, launcher, census, sentinel, &record, error,
        )),
    }
}

/// Abandon a launch's sentinel. Its record is retired only on a fresh proof
/// that the launch ended; whether the publication or the abandon failed, it
/// is otherwise kept for a successor. (Only the intent may be on disk if the
/// sentinel's publication failed; the proof covers either.)
fn abandoned<L: Launcher>(
    journal: &Journal,
    launcher: &mut L,
    census: &impl Census,
    sentinel: L::Sentinel,
    record: &Record,
    error: PluginError,
) -> PluginError {
    match launcher.abandon(sentinel) {
        Ok(()) => match retire_if_ended(journal, record, census) {
            Ok(()) => error,
            Err(kept) => PluginError::Spawn(format!("{error}; {kept}")),
        },
        Err(cleanup) => PluginError::Spawn(format!(
            "{error}; the sentinel was not confirmed ended, so its record is kept: {cleanup}"
        )),
    }
}

/// What one reclaim pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Reclaimed {
    pub(super) busy: bool,
    pub(super) reclaimed: Vec<String>,
    pub(super) stale: Vec<String>,
    pub(super) live: Vec<String>,
    pub(super) unverified: Vec<String>,
}

/// Reclaim every orphaned launch, bounded: at most `limit` records, one
/// group signal per verified record, and its exit observed within `reap`.
pub(super) fn reclaim(
    journal: &Journal,
    census: &impl Census,
    signals: &mut impl Signals,
    own: Own,
    limit: usize,
    reap: Duration,
) -> Result<Reclaimed, PluginError> {
    let mut report = Reclaimed::default();
    let Some(_claim) = journal.claim()? else {
        report.busy = true;
        return Ok(report);
    };
    for scanned in journal.scan(limit)? {
        let record = match scanned {
            Scanned::Record(record) => record,
            Scanned::Unverified(why) => {
                report.unverified.push(why);
                continue;
            }
        };
        match decide(&record, census, own) {
            Decision::LiveOwner => report.live.push(record.nonce),
            Decision::Stale => match prove_ended(&record, census) {
                Some(proof) => {
                    journal.retire_record(proof)?;
                    report.stale.push(record.nonce);
                }
                None => report.unverified.push(format!(
                    "{}: stale, but not proven ended by a fresh census",
                    record.nonce
                )),
            },
            Decision::Unverified(why) => report.unverified.push(why),
            Decision::Reclaim(target) => {
                match signal::signal(Target::RecordedGroup(target), own, census, signals) {
                    Ok(_) => match ended_within(&record, &target, census, reap) {
                        Some(proof) => {
                            journal.retire_record(proof)?;
                            report.reclaimed.push(record.nonce);
                        }
                        None => report.unverified.push(format!(
                            "{}: its group did not end within the reap bound",
                            record.nonce
                        )),
                    },
                    Err(refusal) => report
                        .unverified
                        .push(format!("{}: {refusal}", record.nonce)),
                }
            }
        }
    }
    Ok(report)
}

/// Adopted processes are launchd's to reap: their end is observed, never
/// waited on, within the bound, as a fresh proof about the signalled group's
/// own sentinel (an intent-only record names it only through discovery).
fn ended_within(
    record: &Record,
    target: &RecordedGroup,
    census: &impl Census,
    reap: Duration,
) -> Option<Ended> {
    let observed = Record {
        sentinel: Some(Recorded {
            pid: target.sentinel.pid,
            started: target.sentinel.started,
        }),
        ..record.clone()
    };
    let deadline = tokio::time::Instant::now() + reap;
    loop {
        if let Some(proof) = prove_ended(&observed, census) {
            return Some(proof);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;

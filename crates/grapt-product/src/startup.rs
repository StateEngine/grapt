//! Restart admission for the product-owned service. Discovery is read-only until
//! the candidate has authenticated itself; replacement still consumes the
//! service controller's exact-byte CAS and an elected controller authority.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use grapt_service::{
    ElectedControllerAuthority, LockAttemptPolicy, Observation, ProbeOutcome, RegistrationRecord,
    Retirement, ServiceController, StoreError,
};
use grapt_store::SystemClock;
use grapt_types::service::{
    EnvironmentReceipt, OwnerReceipt, ServiceGeneration, ServiceInstanceId,
    ServiceRegistrationVersion, ServiceSlotId,
};

use crate::compose::ProductRefusal;

const MAX_REGISTRATION_BYTES: u64 = 8_512;

pub(crate) enum Admission {
    Ordinary(Option<ElectedControllerAuthority>),
    Handover(Box<crate::handover::Validated>),
}

impl Admission {
    pub(crate) fn expected_build(&self) -> Option<&str> {
        match self {
            Self::Handover(proof) => Some(proof.expected_build()),
            Self::Ordinary(_) => None,
        }
    }
    pub(crate) fn prepare_platform(
        self,
        root: &Path,
    ) -> Result<(grapt_platform::PlatformOwner, Self), ProductRefusal> {
        let expected = self.expected_build();
        let platform = grapt_platform::PlatformOwner::prepare_for_build(root, expected)
            .map_err(crate::installation::platform_refusal)?;
        let admission = match self {
            Self::Handover(proof) => Self::Handover(Box::new(proof.measure_store(&platform)?)),
            ordinary => ordinary,
        };
        Ok((platform, admission))
    }
}

pub(crate) fn acquire(root: &Path) -> Result<ElectedControllerAuthority, ProductRefusal> {
    let policy =
        LockAttemptPolicy::new(3).ok_or_else(|| refused("registration lock bound was zero"))?;
    ElectedControllerAuthority::acquire(root.join("registration"), policy)
        .map_err(|error| refused(error.to_string()))
}

pub(crate) struct StartupSnapshot {
    bytes: Option<Vec<u8>>,
    incumbent: Option<OwnerReceipt>,
    generation: ServiceGeneration,
}

impl StartupSnapshot {
    /// This is only a generation hint. The elected controller must re-read and
    /// compare these exact bytes before it can admit a replacement.
    pub(crate) fn read(
        root: &Path,
        slot: &ServiceSlotId,
        environment: &EnvironmentReceipt,
    ) -> Result<Self, ProductRefusal> {
        let mut bytes = Vec::new();
        match File::open(root.join("registration")) {
            Ok(file) => {
                file.take(MAX_REGISTRATION_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| refused(format!("registration read failed: {error}")))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    bytes: None,
                    incumbent: None,
                    generation: ServiceGeneration::FIRST,
                });
            }
            Err(error) => return Err(refused(format!("registration read failed: {error}"))),
        }
        let record = RegistrationRecord::decode(&bytes)
            .map_err(|error| refused(format!("registration decode failed: {error}")))?;
        let incumbent = record.owner();
        if incumbent.slot() != slot || incumbent.environment() != environment {
            return Err(refused(
                "registration belongs to another slot or environment",
            ));
        }
        let generation = incumbent
            .generation()
            .next()
            .ok_or_else(|| refused("registration generation is exhausted"))?;
        Ok(Self {
            bytes: Some(bytes),
            incumbent: Some(incumbent.clone()),
            generation,
        })
    }

    pub(crate) fn generation(&self) -> ServiceGeneration {
        self.generation
    }

    pub(crate) fn instance(&self, configured: &str) -> Result<ServiceInstanceId, ProductRefusal> {
        let instance = if self.incumbent.is_some() {
            format!("{configured}-restart-{}", self.generation.value())
        } else {
            configured.to_owned()
        };
        ServiceInstanceId::new(instance).ok_or_else(|| refused("restart instance is invalid"))
    }

    /// Read-only launch advice, never retirement authority. Actual startup
    /// repeats discovery, dead-process observation and exact-byte controller CAS.
    pub(crate) async fn inspect(
        root: &Path,
        slot: &str,
        environment: &str,
        instance: &str,
    ) -> Result<(u64, String), ProductRefusal> {
        let slot = ServiceSlotId::new(slot).ok_or_else(|| refused("invalid startup slot"))?;
        let environment = EnvironmentReceipt::new(environment)
            .ok_or_else(|| refused("invalid startup environment"))?;
        let snapshot = Self::read(root, &slot, &environment)?;
        if let Some(owner) = snapshot.incumbent.clone() {
            let dead = DeadPredecessor::observe(owner, root).await?;
            crate::ready::check_dead_predecessor(root, &dead)
                .map_err(|error| ProductRefusal::Custody(error.to_string()))?;
        } else if root
            .join(crate::ready::READY_RECEIPT_FILE)
            .try_exists()
            .map_err(|error| refused(format!("ready receipt observation failed: {error}")))?
        {
            return Err(refused("ready receipt has no attributable registration"));
        }
        Ok((
            snapshot.generation.value(),
            snapshot.instance(instance)?.as_str().to_owned(),
        ))
    }

    pub(crate) async fn commit(
        self,
        root: &Path,
        declared: &OwnerReceipt,
        probe: ProbeOutcome,
        version: &ServiceRegistrationVersion,
    ) -> Result<ElectedControllerAuthority, ProductRefusal> {
        self.commit_elected(root, declared, probe, version, acquire(root)?)
            .await
    }

    pub(crate) async fn commit_elected(
        self,
        root: &Path,
        declared: &OwnerReceipt,
        probe: ProbeOutcome,
        version: &ServiceRegistrationVersion,
        authority: ElectedControllerAuthority,
    ) -> Result<ElectedControllerAuthority, ProductRefusal> {
        let clock = SystemClock;
        let controller = ServiceController::new(
            declared.slot().clone(),
            declared.environment().clone(),
            &authority,
            &clock,
        );
        let discovered = controller
            .discover()
            .map_err(|error| refused(error.to_string()))?;
        if discovered.bytes() != self.bytes.as_deref() {
            return Err(refused("registration changed after startup discovery"));
        }
        let observation = Observation::new(declared.clone(), probe, version.clone());
        let prepared = controller
            .prepare(&discovered, &observation)
            .map_err(|error| refused(error.to_string()))?;
        let retirement = if let Some(incumbent) = self.incumbent {
            // This path holds (or just took) the election authority, so the
            // held lock is itself the observed absence on every OS: a live
            // predecessor would hold the same election lock.
            let dead = DeadPredecessor::observed_dead(incumbent);
            crate::handover::crash_boundary(crate::handover::CrashBoundary::Recovery);
            // Remove only this dead predecessor's discovery receipt, before
            // the CAS. A crash on either side of the CAS therefore leaves an
            // absent receipt, never an older receipt blocking its successor.
            crate::ready::retire_dead_predecessor(root, &dead)
                .map_err(|error| ProductRefusal::Custody(error.to_string()))?;
            Retirement::Retired
        } else {
            Retirement::NoIncumbent
        };
        let switch = controller
            .commit(prepared, retirement)
            .map_err(|error| refused(error.to_string()))?;
        if switch.committed_owner() != Some(declared) {
            return Err(refused("registration CAS did not commit the probed owner"));
        }
        Ok(authority)
    }
}

/// No public constructor: the ready-file recovery path requires a
/// predecessor observed dead through the service's own election, in
/// addition to the controller's registration custody.
pub(crate) struct DeadPredecessor(OwnerReceipt);

/// Wait for a registered owner to actually go away, up to a bounded number of
/// observations.
///
/// [`DeadPredecessor::observe`] answers "is it gone right now" and refuses if
/// not. A handover needs the stronger thing: the incumbent is going down and we
/// must not proceed until it HAS, because the writes it makes on the way out
/// are exactly the ones a stale source observation would miss.
///
/// "Gone" is the service's own fact, not a process-table lookup: the
/// incumbent holds the elected-registration lock for its whole service life
/// and the kernel releases it when the process dies. A pid that is still
/// present under another owner (pid reuse, pid namespaces) therefore says
/// nothing about the incumbent, and this waits for the lock — not for the
/// pid — to come free.
///
/// Returns [`Retirement::TimedOut`] rather than an error when the bound
/// expires. That is not a failure to report later — it is the fact the
/// transition needs, because a process that would not go away is precisely the
/// one that may still be writing, and `QuiescedSource` refuses it by name.
pub(crate) async fn await_retirement(
    _owner: &OwnerReceipt,
    attempts: u32,
    interval: std::time::Duration,
    service_root: &Path,
) -> Result<Retirement, ProductRefusal> {
    for _ in 0..attempts {
        if !election_lock_held(service_root)? {
            return Ok(Retirement::Retired);
        }
        tokio::time::sleep(interval).await;
    }
    if election_lock_held(service_root)? {
        return Ok(Retirement::TimedOut);
    }
    Ok(Retirement::Retired)
}

impl DeadPredecessor {
    /// Observed absence, not a request to stop. The handover operation
    /// depends on this distinction: a process that ignored a stop is exactly
    /// the one that may still write to the source.
    ///
    /// The caller must not already hold this service's election. A live
    /// predecessor holds the elected-registration lock for its whole service
    /// life, so the lock — not the process table — is what says "still
    /// present": the kernel releases the lock when the process dies, whatever
    /// the process table then says about a pid that has since been recycled.
    pub(crate) async fn observe(
        owner: OwnerReceipt,
        service_root: &Path,
    ) -> Result<Self, ProductRefusal> {
        if election_lock_held(service_root)? {
            return Err(refused(format!(
                "registered process {} is still present; restart cannot retire it",
                owner.pid().value()
            )));
        }
        Ok(Self(owner))
    }

    /// The caller already holds this service's elected controller authority.
    /// A live predecessor would hold the same election lock, so the held
    /// authority is itself the observed absence; no second observation can
    /// add a fact here, and /proc may say the opposite, because a pid is a
    /// number the kernel recycles rather than an identity.
    pub(crate) fn observed_dead(owner: OwnerReceipt) -> Self {
        Self(owner)
    }

    pub(crate) fn owner(&self) -> &OwnerReceipt {
        &self.0
    }
}

/// Whether the elected-registration lock is held, probed with a single
/// non-blocking attempt.
///
/// The probe reuses the production acquisition with a one-attempt budget: a
/// `WouldBlock` means some other handle holds the lock, and success drops the
/// probe's lock at once, leaving the floor exactly as found. The lock is a
/// real OS lock on every platform — `flock` on Unix, `LockFileEx` on Windows,
/// through `std::fs::File::try_lock` — so the kernel releases it when the
/// holder's process dies, and the probe's own fresh handle is denied while
/// any holder keeps it, even a holder in this same process. It must therefore
/// run where this process holds no election of its own; such a caller already
/// has the stronger fact (its authority) through
/// [`DeadPredecessor::observed_dead`].
fn election_lock_held(service_root: &Path) -> Result<bool, ProductRefusal> {
    let policy =
        LockAttemptPolicy::new(1).ok_or_else(|| refused("registration lock bound was zero"))?;
    match ElectedControllerAuthority::acquire(service_root.join("registration"), policy) {
        Ok(probe) => {
            drop(probe);
            Ok(false)
        }
        Err(StoreError::LockBudgetExhausted { .. }) => Ok(true),
        Err(error) => Err(refused(format!("election lock probe failed: {error}"))),
    }
}

fn refused(reason: impl Into<String>) -> ProductRefusal {
    ProductRefusal::Registration(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::authority::Timestamp;
    use grapt_types::service::{Endpoint, ProcessId};

    fn owner() -> OwnerReceipt {
        owner_with_pid(std::process::id())
    }

    fn owner_with_pid(pid: u32) -> OwnerReceipt {
        OwnerReceipt::new(
            ServiceSlotId::new("product").unwrap_or_else(|| panic!("slot")),
            ServiceInstanceId::new("original").unwrap_or_else(|| panic!("instance")),
            EnvironmentReceipt::new("test").unwrap_or_else(|| panic!("environment")),
            ServiceGeneration::new(7),
            ProcessId::new(pid).unwrap_or_else(|| panic!("pid")),
            Endpoint::new("http://127.0.0.1:41111").unwrap_or_else(|| panic!("endpoint")),
        )
    }

    /// A live pid under a DIFFERENT process: pids are numbers the kernel
    /// recycles, so the witnesses use a pid that is known not to be the
    /// incumbent's and is alive while the test runs.
    /// A witness process the test owns until drop: killed and reaped in
    /// `Drop`, so a failing assertion cannot orphan it. Spawned directly
    /// (no shell wrapper) so the killed pid is the one the owner receipt
    /// names — on Windows a `cmd /c` wrapper would die and leave its
    /// `ping` child alive.
    struct OwnedChild(std::process::Child);

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn owner_with_unrelated_live_pid() -> (OwnerReceipt, OwnedChild) {
        let mut command = if cfg!(windows) {
            let mut command = std::process::Command::new("ping");
            command.args(["-n", "120", "127.0.0.1"]);
            command
        } else {
            let mut command = std::process::Command::new("sleep");
            command.arg("120");
            command
        };
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        let child = command
            .spawn()
            .unwrap_or_else(|error| panic!("unrelated process spawn failed: {error}"));
        (owner_with_pid(child.id()), OwnedChild(child))
    }

    /// Witness 1 (unit): a registration that names a live pid under a
    /// different process while nobody holds the election lock is a dead
    /// predecessor. The process table says the pid is present; the lock says
    /// the owner is gone, and the lock is the law.
    #[tokio::test]
    async fn a_lock_free_registration_is_dead_despite_a_live_pid() {
        let root =
            std::env::temp_dir().join(format!("grapt-startup-lock-free-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap_or_else(|error| panic!("fixture root: {error}"));
        let (owner, child) = owner_with_unrelated_live_pid();
        assert!(
            DeadPredecessor::observe(owner, &root).await.is_ok(),
            "a lock-free predecessor must be observed dead whatever the process table says"
        );
        drop(child);
        std::fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }

    /// Witness 2 (unit): the same registration with the election lock HELD is
    /// a live predecessor and must be refused — even though the registered
    /// pid is alive under an unrelated process and would have passed the old
    /// process-presence judgment. The holder is this test process and the
    /// probe is a separate handle, so the answer comes from the lock, not
    /// from the pid.
    #[tokio::test]
    async fn a_held_election_lock_keeps_the_predecessor_alive() {
        let root =
            std::env::temp_dir().join(format!("grapt-startup-lock-held-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap_or_else(|error| panic!("fixture root: {error}"));
        let (owner, child) = owner_with_unrelated_live_pid();
        let holder = hold_election_lock(&root);
        let refused = DeadPredecessor::observe(owner, &root).await;
        drop(holder);
        assert!(
            matches!(
                refused,
                Err(ProductRefusal::Registration(reason)) if reason.contains("still present")
            ),
            "a lock-holding predecessor must be refused"
        );
        drop(child);
        std::fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }

    /// await_retirement observes the lock, not the pid: the bound expires
    /// while the lock is held, and the retirement is admitted the moment the
    /// lock is free while the (unrelated, live) registered pid still is.
    #[tokio::test]
    async fn await_retirement_waits_on_the_lock_not_the_pid() {
        let root = std::env::temp_dir().join(format!("grapt-startup-await-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap_or_else(|error| panic!("fixture root: {error}"));
        let (owner, child) = owner_with_unrelated_live_pid();
        let held_root = root.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let holder = std::thread::spawn(move || {
            let lock = hold_election_lock(&held_root);
            ready_tx
                .send(())
                .unwrap_or_else(|error| panic!("ready signal: {error}"));
            // The timeout assertion below happens BEFORE this release: a
            // delayed test thread can no longer race the release and
            // invalidate the witness. The bounded backstop only matters if
            // the test thread panicked (its `release_tx` drop then fails
            // this receive at once); the lock must never outlive the test.
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
            lock.unlock()
                .unwrap_or_else(|error| panic!("election unlock: {error}"));
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("lock holder did not start: {error}"));
        let timed_out = await_retirement(&owner, 2, std::time::Duration::from_millis(50), &root)
            .await
            .unwrap_or_else(|error| panic!("await_retirement: {error}"));
        assert_eq!(timed_out, Retirement::TimedOut);
        release_tx
            .send(())
            .unwrap_or_else(|error| panic!("release signal: {error}"));
        holder
            .join()
            .unwrap_or_else(|error| panic!("holder thread: {error:?}"));
        let retired = await_retirement(&owner, 5, std::time::Duration::from_millis(50), &root)
            .await
            .unwrap_or_else(|error| panic!("await_retirement: {error}"));
        assert_eq!(retired, Retirement::Retired);
        drop(child);
        std::fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }

    // O_CLOEXEC: a lock held across `fork`+`exec` by an inherited file
    // description survives the holder's own drop, so the holder's fd must
    // never cross into a child another test may spawn in parallel. Windows
    // handles are not inherited by default, so no flag is needed there.
    #[cfg(unix)]
    const LOCK_FD_CLOSE_ON_EXEC: i32 = 0o2000000;

    fn hold_election_lock(root: &Path) -> std::fs::File {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(LOCK_FD_CLOSE_ON_EXEC);
        }
        let file = options
            .open(root.join("registration.elected"))
            .unwrap_or_else(|error| panic!("election lock open failed: {error}"));
        file.try_lock()
            .unwrap_or_else(|error| panic!("election lock hold failed: {error:?}"));
        file
    }

    #[tokio::test]
    async fn startup_snapshot_cannot_overwrite_changed_or_corrupt_registration() {
        let root =
            std::env::temp_dir().join(format!("grapt-startup-snapshot-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap_or_else(|error| panic!("fixture root: {error}"));
        assert_eq!(
            StartupSnapshot::inspect(&root, "product", "internal-alpha", "desktop-new")
                .await
                .unwrap_or_else(|error| panic!("{error}")),
            (0, "desktop-new".to_owned())
        );
        std::fs::write(root.join(crate::ready::READY_RECEIPT_FILE), b"{}")
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            StartupSnapshot::inspect(&root, "product", "internal-alpha", "desktop-new")
                .await
                .is_err(),
            "unattributed receipt was admitted"
        );
        std::fs::remove_file(root.join(crate::ready::READY_RECEIPT_FILE))
            .unwrap_or_else(|error| panic!("{error}"));
        let original = owner();
        let version = ServiceRegistrationVersion::new("0.1.0").unwrap_or_else(|| panic!("version"));
        let record =
            RegistrationRecord::new(original.clone(), version.clone(), Timestamp::from_millis(0))
                .unwrap_or_else(|error| panic!("record: {error}"));
        let bytes = record.encode();
        let path = root.join("registration");
        std::fs::write(&path, &bytes).unwrap_or_else(|error| panic!("registration: {error}"));
        {
            // The law on every OS: the same registration is refused only
            // while the election lock is held, and admitted the moment it is
            // free — whatever the process table says about the live,
            // unrelated pid.
            let holder = hold_election_lock(&root);
            assert!(
                StartupSnapshot::inspect(
                    &root,
                    original.slot().as_str(),
                    original.environment().as_str(),
                    "desktop-new"
                )
                .await
                .is_err(),
                "inspection admitted a lock-holding predecessor"
            );
            // A sibling fork can retain this same open file description
            // until exec. Keep a duplicate to make that window deterministic,
            // and explicitly unlock rather than assuming drop releases it.
            let inherited = holder
                .try_clone()
                .unwrap_or_else(|error| panic!("inherited election fd: {error}"));
            holder
                .unlock()
                .unwrap_or_else(|error| panic!("election unlock: {error}"));
            drop(holder);
            assert!(
                StartupSnapshot::inspect(
                    &root,
                    original.slot().as_str(),
                    original.environment().as_str(),
                    "desktop-new"
                )
                .await
                .is_ok(),
                "lock-free stale predecessor must be admitted"
            );
            drop(inherited);
        }
        assert_eq!(
            std::fs::read(&path).unwrap_or_else(|error| panic!("{error}")),
            bytes
        );
        let snapshot = StartupSnapshot::read(&root, original.slot(), original.environment())
            .unwrap_or_else(|error| panic!("snapshot: {error}"));
        assert_eq!(snapshot.generation().value(), 8);
        assert_eq!(
            snapshot
                .instance("original")
                .unwrap_or_else(|error| panic!("{error}"))
                .as_str(),
            "original-restart-8"
        );
        std::fs::write(&path, b"torn registration")
            .unwrap_or_else(|error| panic!("torn fixture: {error}"));
        let refused = snapshot
            .commit(
                &root,
                &original,
                ProbeOutcome::Attributed(original.clone()),
                &version,
            )
            .await;
        assert!(
            matches!(refused, Err(ProductRefusal::Registration(reason)) if reason.contains("changed after startup"))
        );
        assert_eq!(
            std::fs::read(&path).unwrap_or_else(|error| panic!("readback: {error}")),
            b"torn registration"
        );
        assert!(StartupSnapshot::read(&root, original.slot(), original.environment()).is_err());
        std::fs::write(&path, vec![b'x'; 8_513])
            .unwrap_or_else(|error| panic!("oversize fixture: {error}"));
        assert!(StartupSnapshot::read(&root, original.slot(), original.environment()).is_err());
        std::fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }
}

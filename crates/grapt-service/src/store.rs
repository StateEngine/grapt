//! Bounded, capability-carried access to the discovery registration.
//!
//! A production mutation requires two things which cannot be forged from raw
//! paths: an [`ElectedControllerAuthority`] that still holds its election lock,
//! and the crate-private permit retained inside that authority. The backing
//! store remains inspectable, but its CAS cannot be invoked by an arbitrary
//! external dependent.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use thiserror::Error;

use crate::registration::MAX_REGISTRATION_BYTES;

const MAX_REGISTRATION_BYTES_U64: u64 = 8_512;

/// A finite acquisition budget. No lock operation in this module blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockAttemptPolicy {
    maximum: NonZeroU32,
}

impl LockAttemptPolicy {
    pub fn new(maximum: u32) -> Option<Self> {
        NonZeroU32::new(maximum).map(|maximum| Self { maximum })
    }

    pub fn maximum(self) -> u32 {
        self.maximum.get()
    }
}

/// Evidence that the bytes were changed before explicit unlock failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostCommitUnlockWarning {
    path: PathBuf,
    error_kind: std::io::ErrorKind,
}

impl PostCommitUnlockWarning {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn error_kind(&self) -> std::io::ErrorKind {
        self.error_kind
    }
}

/// The result of a compare-and-swap against exact slot contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasOutcome {
    /// The external effect happened. An unlock warning cannot erase that fact.
    Committed {
        unlock_warning: Option<PostCommitUnlockWarning>,
    },
    /// The expected bytes were absent and the slot was left untouched.
    Lost { observed: Option<Vec<u8>> },
}

/// Why the registration authority or bounded store could not progress.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("registration slot {path:?} could not be read: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("registration slot {path:?} could not be written: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the lock for registration slot {path:?} could not be opened or acquired: {source}")]
    Lock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "the lock for registration slot {path:?} remained contended for all {attempts} finite attempts"
    )]
    LockBudgetExhausted { path: PathBuf, attempts: u32 },
    #[error(
        "registration slot {path:?} contains {found} bytes, above the finite {maximum}-byte read bound"
    )]
    RegistrationTooLarge {
        path: PathBuf,
        maximum: usize,
        found: u64,
    },
    #[error("the supplied mutation permit does not belong to this elected authority")]
    WrongAuthority,
    #[error("the in-memory registration slot was poisoned by a panic in another thread")]
    Poisoned,
    #[error("the injected write fault fired after the compare succeeded but before any write")]
    InjectedWriteFault,
    #[error("this test-only operation requires an isolated in-memory authority")]
    NotIsolated,
}

/// Opaque proof carried only inside one elected authority.
///
/// The type is public so a public sealed trait can name it, but it has no
/// public constructor and the authority's accessor is crate-private.
#[derive(Debug)]
pub struct RegistrationMutationPermit {
    _private: (),
}

mod sealed {
    pub trait Sealed {}
}

/// A byte store whose mutation requires an opaque permit.
///
/// External crates can name the trait for read-only generic code, but it is
/// sealed and cannot be implemented outside this crate.
pub trait RegistrationStore: sealed::Sealed + core::fmt::Debug {
    fn read(&self) -> Result<Option<Vec<u8>>, StoreError>;

    fn compare_and_swap(
        &self,
        permit: &RegistrationMutationPermit,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
    ) -> Result<CasOutcome, StoreError>;
}

/// File-backed storage. Construction grants read access only; mutation still
/// requires the private permit of an elected authority.
#[derive(Debug)]
pub struct FileRegistrationStore {
    registration: PathBuf,
    lock: PathBuf,
    policy: LockAttemptPolicy,
    fail_next_unlock: AtomicBool,
}

impl FileRegistrationStore {
    pub fn new(registration: impl Into<PathBuf>, policy: LockAttemptPolicy) -> Self {
        let registration = registration.into();
        let lock = suffixed(&registration, ".lock");
        Self {
            registration,
            lock,
            policy,
            fail_next_unlock: AtomicBool::new(false),
        }
    }

    pub fn registration_path(&self) -> &Path {
        &self.registration
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock
    }

    fn acquire(&self) -> Result<File, StoreError> {
        acquire_bounded(&self.lock, self.policy)
    }

    fn read_unlocked(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let file = match File::open(&self.registration) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Read {
                    path: self.registration.clone(),
                    source,
                })
            }
        };
        let advertised = file
            .metadata()
            .map_err(|source| StoreError::Read {
                path: self.registration.clone(),
                source,
            })?
            .len();
        if advertised > MAX_REGISTRATION_BYTES_U64 {
            return Err(StoreError::RegistrationTooLarge {
                path: self.registration.clone(),
                maximum: MAX_REGISTRATION_BYTES,
                found: advertised,
            });
        }
        let capacity =
            usize::try_from(advertised).map_err(|_| StoreError::RegistrationTooLarge {
                path: self.registration.clone(),
                maximum: MAX_REGISTRATION_BYTES,
                found: advertised,
            })?;
        let mut bytes = Vec::with_capacity(capacity);
        file.take(MAX_REGISTRATION_BYTES_U64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| StoreError::Read {
                path: self.registration.clone(),
                source,
            })?;
        if bytes.len() > MAX_REGISTRATION_BYTES {
            return Err(StoreError::RegistrationTooLarge {
                path: self.registration.clone(),
                maximum: MAX_REGISTRATION_BYTES,
                found: usize_as_u64(bytes.len()),
            });
        }
        Ok(Some(bytes))
    }

    fn write_atomically(&self, bytes: &[u8]) -> Result<(), StoreError> {
        if bytes.len() > MAX_REGISTRATION_BYTES {
            return Err(StoreError::RegistrationTooLarge {
                path: self.registration.clone(),
                maximum: MAX_REGISTRATION_BYTES,
                found: usize_as_u64(bytes.len()),
            });
        }
        let temp = suffixed(&self.registration, ".tmp");
        let write = |path: &Path| -> std::io::Result<()> {
            let mut file = File::create(path)?;
            file.write_all(bytes)?;
            file.sync_all()
        };
        write(&temp).map_err(|source| StoreError::Write {
            path: temp.clone(),
            source,
        })?;
        std::fs::rename(&temp, &self.registration).map_err(|source| StoreError::Write {
            path: self.registration.clone(),
            source,
        })
    }

    fn remove(&self) -> Result<(), StoreError> {
        match std::fs::remove_file(&self.registration) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::Write {
                path: self.registration.clone(),
                source,
            }),
        }
    }

    fn unlock(&self, guard: File) -> Result<(), std::io::Error> {
        if self.fail_next_unlock.swap(false, Ordering::SeqCst) {
            drop(guard);
            return Err(std::io::Error::other("injected post-effect unlock failure"));
        }
        guard.unlock()
    }
}

impl sealed::Sealed for FileRegistrationStore {}

impl RegistrationStore for FileRegistrationStore {
    fn read(&self) -> Result<Option<Vec<u8>>, StoreError> {
        self.read_unlocked()
    }

    fn compare_and_swap(
        &self,
        _permit: &RegistrationMutationPermit,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
    ) -> Result<CasOutcome, StoreError> {
        let guard = self.acquire()?;
        let observed = self.read_unlocked()?;
        if observed.as_deref() != expected {
            guard.unlock().map_err(|source| StoreError::Lock {
                path: self.lock.clone(),
                source,
            })?;
            return Ok(CasOutcome::Lost { observed });
        }
        match next {
            Some(bytes) => self.write_atomically(bytes)?,
            None => self.remove()?,
        }
        let unlock_warning = self
            .unlock(guard)
            .err()
            .map(|source| PostCommitUnlockWarning {
                path: self.lock.clone(),
                error_kind: source.kind(),
            });
        Ok(CasOutcome::Committed { unlock_warning })
    }
}

#[derive(Debug, Default)]
struct InMemoryRegistrationStore {
    state: Mutex<InMemoryState>,
}

#[derive(Debug, Default)]
struct InMemoryState {
    contents: Option<Vec<u8>>,
    fail_next_write: bool,
    fail_next_unlock: bool,
}

impl sealed::Sealed for InMemoryRegistrationStore {}

impl RegistrationStore for InMemoryRegistrationStore {
    fn read(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(state.contents.clone())
    }

    fn compare_and_swap(
        &self,
        _permit: &RegistrationMutationPermit,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
    ) -> Result<CasOutcome, StoreError> {
        let mut state = self.state.lock().map_err(|_| StoreError::Poisoned)?;
        if state.contents.as_deref() != expected {
            return Ok(CasOutcome::Lost {
                observed: state.contents.clone(),
            });
        }
        if core::mem::take(&mut state.fail_next_write) {
            return Err(StoreError::InjectedWriteFault);
        }
        state.contents = next.map(<[u8]>::to_vec);
        let unlock_warning =
            core::mem::take(&mut state.fail_next_unlock).then(|| PostCommitUnlockWarning {
                path: PathBuf::from("isolated-memory.lock"),
                error_kind: std::io::ErrorKind::Other,
            });
        Ok(CasOutcome::Committed { unlock_warning })
    }
}

#[derive(Debug)]
enum StoreBackend {
    File(FileRegistrationStore),
    Isolated(InMemoryRegistrationStore),
}

impl sealed::Sealed for StoreBackend {}

impl RegistrationStore for StoreBackend {
    fn read(&self) -> Result<Option<Vec<u8>>, StoreError> {
        match self {
            Self::File(store) => store.read(),
            Self::Isolated(store) => store.read(),
        }
    }

    fn compare_and_swap(
        &self,
        permit: &RegistrationMutationPermit,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
    ) -> Result<CasOutcome, StoreError> {
        match self {
            Self::File(store) => store.compare_and_swap(permit, expected, next),
            Self::Isolated(store) => store.compare_and_swap(permit, expected, next),
        }
    }
}

/// Unforgeable carrier for the elected controller's mutation authority.
#[derive(Debug)]
pub struct ElectedControllerAuthority {
    store: StoreBackend,
    permit: RegistrationMutationPermit,
    election_guard: Option<File>,
    election_path: Option<PathBuf>,
}

impl ElectedControllerAuthority {
    /// Acquire the production authority under a finite election-lock budget.
    pub fn acquire(
        registration: impl Into<PathBuf>,
        policy: LockAttemptPolicy,
    ) -> Result<Self, StoreError> {
        let registration = registration.into();
        let election_path = suffixed(&registration, ".elected");
        let election_guard = acquire_bounded(&election_path, policy)?;
        Ok(Self {
            store: StoreBackend::File(FileRegistrationStore::new(registration, policy)),
            permit: RegistrationMutationPermit { _private: () },
            election_guard: Some(election_guard),
            election_path: Some(election_path),
        })
    }

    /// Deterministic isolated authority for contract/corpus tests. It can
    /// mutate no filesystem discovery slot.
    #[doc(hidden)]
    pub fn isolated() -> Self {
        Self {
            store: StoreBackend::Isolated(InMemoryRegistrationStore::default()),
            permit: RegistrationMutationPermit { _private: () },
            election_guard: None,
            election_path: None,
        }
    }

    pub fn registration_path(&self) -> Option<&Path> {
        match &self.store {
            StoreBackend::File(store) => Some(store.registration_path()),
            StoreBackend::Isolated(_) => None,
        }
    }

    pub fn lock_path(&self) -> Option<&Path> {
        match &self.store {
            StoreBackend::File(store) => Some(store.lock_path()),
            StoreBackend::Isolated(_) => None,
        }
    }

    pub fn election_path(&self) -> Option<&Path> {
        self.election_path.as_deref()
    }

    pub fn read_registration(&self) -> Result<Option<Vec<u8>>, StoreError> {
        self.store.read()
    }

    #[doc(hidden)]
    pub fn seed_isolated(&self, contents: Option<Vec<u8>>) -> Result<(), StoreError> {
        let StoreBackend::Isolated(store) = &self.store else {
            return Err(StoreError::NotIsolated);
        };
        if contents
            .as_ref()
            .is_some_and(|bytes| bytes.len() > MAX_REGISTRATION_BYTES)
        {
            return Err(StoreError::RegistrationTooLarge {
                path: PathBuf::from("isolated-memory"),
                maximum: MAX_REGISTRATION_BYTES,
                found: contents
                    .as_ref()
                    .map_or(0, |bytes| usize_as_u64(bytes.len())),
            });
        }
        let mut state = store.state.lock().map_err(|_| StoreError::Poisoned)?;
        state.contents = contents;
        Ok(())
    }

    #[doc(hidden)]
    pub fn fail_next_isolated_write(&self) -> Result<(), StoreError> {
        let StoreBackend::Isolated(store) = &self.store else {
            return Err(StoreError::NotIsolated);
        };
        let mut state = store.state.lock().map_err(|_| StoreError::Poisoned)?;
        state.fail_next_write = true;
        Ok(())
    }

    #[doc(hidden)]
    pub fn fail_next_unlock(&self) -> Result<(), StoreError> {
        match &self.store {
            StoreBackend::File(store) => {
                store.fail_next_unlock.store(true, Ordering::SeqCst);
                Ok(())
            }
            StoreBackend::Isolated(store) => {
                let mut state = store.state.lock().map_err(|_| StoreError::Poisoned)?;
                state.fail_next_unlock = true;
                Ok(())
            }
        }
    }

    pub(crate) fn mutation_permit(&self) -> &RegistrationMutationPermit {
        &self.permit
    }

    pub(crate) fn compare_and_swap(
        &self,
        permit: &RegistrationMutationPermit,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
    ) -> Result<CasOutcome, StoreError> {
        if !core::ptr::eq(permit, &self.permit) {
            return Err(StoreError::WrongAuthority);
        }
        self.store.compare_and_swap(permit, expected, next)
    }
}

impl Drop for ElectedControllerAuthority {
    fn drop(&mut self) {
        if let Some(guard) = self.election_guard.take() {
            let _ = guard.unlock();
        }
    }
}

fn acquire_bounded(path: &Path, policy: LockAttemptPolicy) -> Result<File, StoreError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|source| StoreError::Lock {
            path: path.to_path_buf(),
            source,
        })?;
    for attempt in 1..=policy.maximum() {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if attempt < policy.maximum() => {
                std::thread::yield_now();
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(StoreError::LockBudgetExhausted {
                    path: path.to_path_buf(),
                    attempts: policy.maximum(),
                });
            }
            Err(std::fs::TryLockError::Error(source)) => {
                return Err(StoreError::Lock {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
    Err(StoreError::LockBudgetExhausted {
        path: path.to_path_buf(),
        attempts: policy.maximum(),
    })
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_owned();
    raw.push(suffix);
    PathBuf::from(raw)
}

fn usize_as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

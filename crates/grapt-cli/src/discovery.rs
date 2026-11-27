//! Read-only service discovery with an attributed probe and split-brain check.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use grapt_service::{
    AuthenticatedHealthProbe, FileRegistrationStore, HealthExpectation, LockAttemptPolicy,
    RegistrationRecord, RegistrationStore,
};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_CREDENTIAL_BYTES: usize = 512;
const MAX_READY_RECEIPT_BYTES: usize = 8_192;

#[derive(Clone)]
pub struct ClientCredential {
    username: String,
    secret: String,
}

impl fmt::Debug for ClientCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientCredential")
            .field("username", &"<redacted>")
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredService {
    registration: RegistrationRecord,
    credential: ClientCredential,
    build: String,
}

impl DiscoveredService {
    #[cfg(feature = "loopback-testkit")]
    pub fn loopback_testkit(registration: RegistrationRecord) -> Self {
        Self {
            registration,
            build: "testkit".into(),
            credential: ClientCredential {
                username: "test".to_owned(),
                secret: "test".to_owned(),
            },
        }
    }

    /// The same versioned health identity authenticated during discovery.
    pub fn product_health(&self) -> Result<grapt_api::ProductHealth, DiscoveryError> {
        let owner = self.registration.owner();
        grapt_api::ProductHealth::new(
            owner.pid().value(),
            &self.build,
            owner.environment().as_str(),
            owner.instance().as_str(),
        )
        .map_err(|_| DiscoveryError::ReadyMismatch)
    }

    pub fn registration(&self) -> &RegistrationRecord {
        &self.registration
    }

    pub fn endpoint(&self) -> &str {
        self.registration.owner().endpoint().as_str()
    }

    pub fn credential(&self) -> &ClientCredential {
        &self.credential
    }

    pub fn status_receipt(&self) -> ServiceStatusReceipt<'_> {
        ServiceStatusReceipt {
            registration: &self.registration,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ServiceStatusReceipt<'a> {
    registration: &'a RegistrationRecord,
}

impl Serialize for ServiceStatusReceipt<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let owner = self.registration.owner();
        let mut state = serializer.serialize_struct("ServiceStatusReceipt", 9)?;
        state.serialize_field("schema", "secode.cli.service-status.v1")?;
        state.serialize_field("outcome", "ready")?;
        state.serialize_field("serviceSlot", owner.slot().as_str())?;
        state.serialize_field("serviceInstance", owner.instance().as_str())?;
        state.serialize_field("environment", owner.environment().as_str())?;
        state.serialize_field("generation", &owner.generation().value())?;
        state.serialize_field("pid", &owner.pid().value())?;
        state.serialize_field("endpoint", owner.endpoint().as_str())?;
        state.serialize_field("registrationVersion", self.registration.version().as_str())?;
        state.end()
    }
}

#[derive(Debug, Clone)]
pub struct ServiceDiscovery {
    service_root: PathBuf,
    expected_version: String,
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("service registration is absent")]
    Absent,
    #[error("service discovery file {path:?} is unreadable: {kind:?}")]
    Unreadable {
        path: PathBuf,
        kind: std::io::ErrorKind,
    },
    #[error("service discovery file {path:?} is not a regular file")]
    NotRegular { path: PathBuf },
    #[error("service credential is malformed")]
    Credential,
    #[error("service registration is malformed: {0}")]
    Registration(String),
    #[error("service registration protocol is incompatible")]
    IncompatibleProtocol,
    #[error("service health could not be attributed: {0}")]
    Stale(String),
    #[error("service registration changed during attribution")]
    SplitBrain,
    #[error("service ready receipt is absent")]
    ReadyAbsent,
    #[error("service ready receipt is malformed or does not match registration identity")]
    ReadyMismatch,
    #[error("expected registration version is empty or oversized")]
    InvalidVersion,
    #[error("spawned child pid {expected} does not own discovered service pid {found}")]
    ChildOwnerMismatch { expected: u32, found: u32 },
}

/// The launch window: a launched child has this long to publish its own
/// attributed readiness. The deadline bounds the wait's yielding work (pending
/// backoffs and probes); the synchronous segments around each probe (liveness
/// check, byte-bounded registration/receipt/credential reads) run to
/// completion, and the deadline is re-checked after them so overrun is never
/// accepted as readiness. Replaces the former 128-poll count, whose wall time
/// scaled with the same disk latency that delays a legitimate startup.
pub const LAUNCH_WINDOW: Duration = Duration::from_secs(30);
pub const LAUNCH_POLL_INTERVAL: Duration = Duration::from_millis(25);

impl DiscoveryError {
    /// Whether a launch keeps polling: the child has not yet published, or not
    /// yet finished publishing, its own attributed readiness. The product writes
    /// its registration and then its ready receipt, each durably, so a poll
    /// between the two sees `ReadyAbsent`. That is a publication in progress, not
    /// a refusal; on a slow disk the gap is seconds wide.
    pub fn is_launch_pending(&self) -> bool {
        matches!(
            self,
            Self::Absent | Self::Stale(_) | Self::ChildOwnerMismatch { .. } | Self::ReadyAbsent
        )
    }
}

/// What the caller's status read says about a held child between polls.
/// `Exited` means the read returned a status, so the child is already reaped;
/// `StatusFailed` means its state is unknown and the caller must kill and reap
/// it before refusing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildLiveness {
    /// The child still runs.
    Alive,
    /// The status read proved the exit. The message is the caller's.
    Exited(String),
    /// The status read failed. The message is the caller's.
    StatusFailed(String),
}

/// Why a launched child's attributed readiness was not established.
#[derive(Debug)]
pub enum ReadinessWait {
    /// The child's status check proved its exit; nothing is left to clean.
    /// The message is the caller's.
    Child(String),
    /// The child's status check failed; the caller must kill and reap the
    /// child before refusing. The message is the caller's.
    StatusFailed(String),
    /// Discovery refused with an error that further polling cannot change.
    Discovery(DiscoveryError),
    /// The finite launch window ended without the child's readiness, and the
    /// deadline's final liveness check found the child alive; the caller
    /// kills and reaps it.
    Window(LaunchWindowObservation),
}

/// What the launch window saw as it ended, so the refusal names its evidence
/// instead of a bare bound.
#[derive(Debug)]
pub struct LaunchWindowObservation {
    /// Probes that completed before the deadline; a probe the deadline cut
    /// off in flight is not counted.
    polls: u16,
    /// The refusal the last completed pending probe saw, if any probe
    /// completed pending.
    last_pending: Option<DiscoveryError>,
}

impl LaunchWindowObservation {
    pub fn polls(&self) -> u16 {
        self.polls
    }

    pub fn last_pending(&self) -> Option<&DiscoveryError> {
        self.last_pending.as_ref()
    }

    /// Test-visible construction; the production path is the launch
    /// window's own end-of-wait report.
    #[cfg(test)]
    pub(crate) fn for_test(polls: u16, last_pending: Option<DiscoveryError>) -> Self {
        Self {
            polls,
            last_pending,
        }
    }
}

/// The window's report, decided by one final liveness sample: the refusal
/// names the child's state at the moment of refusal, not up to a probe
/// allowance earlier, and a child that died in the last allowance is
/// reported as its exit, not as the window.
fn windowed<F>(mut child_alive: F, observation: LaunchWindowObservation) -> ReadinessWait
where
    F: FnMut() -> ChildLiveness,
{
    match child_alive() {
        ChildLiveness::Alive => ReadinessWait::Window(observation),
        ChildLiveness::Exited(message) => ReadinessWait::Child(message),
        ChildLiveness::StatusFailed(message) => ReadinessWait::StatusFailed(message),
    }
}

impl ServiceDiscovery {
    /// The one launch-window wait: poll for `child_pid`'s own attributed
    /// readiness, checking `child_alive` before every poll, until
    /// [`LAUNCH_WINDOW`] expires. `Exited` ends the wait at once (the status
    /// read already reaped the child); `StatusFailed` ends it for the caller
    /// to kill and reap. Pending backoffs and in-flight probes are both cut at
    /// the deadline; one final liveness sample decides the report there.
    pub async fn await_child_readiness<F>(
        &self,
        child_pid: u32,
        mut child_alive: F,
    ) -> Result<DiscoveredService, ReadinessWait>
    where
        F: FnMut() -> ChildLiveness,
    {
        let deadline = tokio::time::Instant::now() + LAUNCH_WINDOW;
        let mut polls = 0u16;
        let mut last_pending: Option<DiscoveryError> = None;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(windowed(
                    child_alive,
                    LaunchWindowObservation {
                        polls,
                        last_pending,
                    },
                ));
            }
            match child_alive() {
                ChildLiveness::Alive => {}
                ChildLiveness::Exited(message) => return Err(ReadinessWait::Child(message)),
                ChildLiveness::StatusFailed(message) => {
                    return Err(ReadinessWait::StatusFailed(message))
                }
            }
            // Anchor the probe at the deadline itself, not at the pre-liveness
            // sample, so a slow liveness check is never refunded.
            match tokio::time::timeout_at(deadline, self.discover_for_child(child_pid)).await {
                Ok(Ok(service)) => {
                    // The probe's synchronous tail can finish past the deadline.
                    if tokio::time::Instant::now() >= deadline {
                        return Err(windowed(
                            child_alive,
                            LaunchWindowObservation {
                                polls,
                                last_pending,
                            },
                        ));
                    }
                    return Ok(service);
                }
                Ok(Err(error)) if error.is_launch_pending() => {
                    polls = polls.saturating_add(1);
                    last_pending = Some(error);
                    // Clamp the backoff to the deadline too.
                    let _clamped =
                        tokio::time::timeout_at(deadline, tokio::time::sleep(LAUNCH_POLL_INTERVAL))
                            .await;
                }
                Ok(Err(error)) => return Err(ReadinessWait::Discovery(error)),
                Err(_elapsed) => {
                    return Err(windowed(
                        child_alive,
                        LaunchWindowObservation {
                            polls,
                            last_pending,
                        },
                    ))
                }
            }
        }
    }

    pub fn verify_child_owner(
        registration: &RegistrationRecord,
        expected_pid: u32,
    ) -> Result<(), DiscoveryError> {
        let found = registration.owner().pid().value();
        if found == expected_pid {
            Ok(())
        } else {
            Err(DiscoveryError::ChildOwnerMismatch {
                expected: expected_pid,
                found,
            })
        }
    }

    pub fn try_new(
        service_root: impl Into<PathBuf>,
        expected_version: impl Into<String>,
    ) -> Result<Self, DiscoveryError> {
        let expected_version = expected_version.into();
        if expected_version.is_empty() || expected_version.len() > 1_024 {
            return Err(DiscoveryError::InvalidVersion);
        }
        Ok(Self {
            service_root: service_root.into(),
            expected_version,
        })
    }

    pub async fn discover(&self) -> Result<DiscoveredService, DiscoveryError> {
        self.discover_inner(None).await
    }

    pub async fn discover_for_child(
        &self,
        expected_pid: u32,
    ) -> Result<DiscoveredService, DiscoveryError> {
        self.discover_inner(Some(expected_pid)).await
    }

    async fn discover_inner(
        &self,
        expected_pid: Option<u32>,
    ) -> Result<DiscoveredService, DiscoveryError> {
        // Everything before the health probe yields is synchronous and
        // byte-bounded; the launch deadline cannot cut it (see LAUNCH_WINDOW).
        let registration_path = self.service_root.join("registration");
        let policy = LockAttemptPolicy::new(1)
            .ok_or_else(|| DiscoveryError::Registration("zero lock attempt bound".to_owned()))?;
        let store = FileRegistrationStore::new(&registration_path, policy);
        let first_bytes = store
            .read()
            .map_err(|error| DiscoveryError::Registration(error.to_string()))?
            .ok_or(DiscoveryError::Absent)?;
        let first = RegistrationRecord::decode(&first_bytes)
            .map_err(|error| DiscoveryError::Registration(error.to_string()))?;
        if first.version().as_str() != self.expected_version {
            return Err(DiscoveryError::IncompatibleProtocol);
        }
        if let Some(expected) = expected_pid {
            Self::verify_child_owner(&first, expected)?;
        }

        let ready_path = self.service_root.join("serve-ready.json");
        let first_ready_bytes = read_regular(&ready_path, MAX_READY_RECEIPT_BYTES)?
            .ok_or(DiscoveryError::ReadyAbsent)?;
        let ready: ReadyReceipt = serde_json::from_slice(&first_ready_bytes)
            .map_err(|_| DiscoveryError::ReadyMismatch)?;
        if !ready.matches(&first) {
            return Err(DiscoveryError::ReadyMismatch);
        }

        let credential = read_credential(&self.service_root.join("basic.credential"))?;
        let expectation = HealthExpectation::new(first.owner().clone(), ready.build.clone())
            .map_err(|error| DiscoveryError::Stale(error.to_string()))?;
        let probe = AuthenticatedHealthProbe::new(
            credential.username.clone(),
            credential.secret.clone(),
            expectation,
        )
        .map_err(|error| DiscoveryError::Stale(error.to_string()))?;
        probe
            .probe()
            .await
            .map_err(|error| DiscoveryError::Stale(error.to_string()))?;

        let second_bytes = store
            .read()
            .map_err(|error| DiscoveryError::Registration(error.to_string()))?
            .ok_or(DiscoveryError::SplitBrain)?;
        let second_ready_bytes = read_regular(&ready_path, MAX_READY_RECEIPT_BYTES)?
            .ok_or(DiscoveryError::SplitBrain)?;
        if first_bytes != second_bytes || first_ready_bytes != second_ready_bytes {
            return Err(DiscoveryError::SplitBrain);
        }
        Ok(DiscoveredService {
            registration: first,
            credential,
            build: ready.build,
        })
    }
}

impl ClientCredential {
    pub fn read(path: &Path) -> Result<Self, DiscoveryError> {
        read_credential(path)
    }

    pub(crate) fn parts(&self) -> (&str, &str) {
        (&self.username, &self.secret)
    }
}

fn read_credential(path: &Path) -> Result<ClientCredential, DiscoveryError> {
    let bytes =
        read_regular(path, MAX_CREDENTIAL_BYTES)?.ok_or_else(|| DiscoveryError::Unreadable {
            path: path.to_path_buf(),
            kind: std::io::ErrorKind::NotFound,
        })?;
    if bytes.contains(&b'\r') {
        return Err(DiscoveryError::Credential);
    }
    let text = core::str::from_utf8(&bytes).map_err(|_| DiscoveryError::Credential)?;
    if text.lines().count() != 1 {
        return Err(DiscoveryError::Credential);
    }
    let line = text.strip_suffix('\n').unwrap_or(text);
    let (username, secret) = line.split_once(':').ok_or(DiscoveryError::Credential)?;
    if username.is_empty() || username.len() > 128 || secret.is_empty() || secret.len() > 256 {
        return Err(DiscoveryError::Credential);
    }
    Ok(ClientCredential {
        username: username.to_owned(),
        secret: secret.to_owned(),
    })
}

fn read_regular(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, DiscoveryError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(DiscoveryError::Unreadable {
                path: path.to_path_buf(),
                kind: error.kind(),
            })
        }
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(DiscoveryError::NotRegular {
            path: path.to_path_buf(),
        });
    }
    if metadata.len() > maximum as u64 {
        return Err(DiscoveryError::Unreadable {
            path: path.to_path_buf(),
            kind: std::io::ErrorKind::InvalidData,
        });
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| DiscoveryError::Unreadable {
        path: path.to_path_buf(),
        kind: std::io::ErrorKind::InvalidData,
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    File::open(path)
        .and_then(|file| file.take(maximum as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| DiscoveryError::Unreadable {
            path: path.to_path_buf(),
            kind: error.kind(),
        })?;
    if bytes.len() > maximum {
        return Err(DiscoveryError::Unreadable {
            path: path.to_path_buf(),
            kind: std::io::ErrorKind::InvalidData,
        });
    }
    Ok(Some(bytes))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyReceipt {
    schema: String,
    operation: String,
    service_slot: String,
    service_instance: String,
    environment: String,
    service_generation: u64,
    pid: u32,
    observed_endpoint: String,
    serve_window_ms: u64,
    build: String,
}

impl ReadyReceipt {
    fn matches(&self, registration: &RegistrationRecord) -> bool {
        let owner = registration.owner();
        self.schema == grapt_api::product_health::PRODUCT_READY_SCHEMA
            && self.operation == "serve"
            && self.service_slot == owner.slot().as_str()
            && self.service_instance == owner.instance().as_str()
            && self.environment == owner.environment().as_str()
            && self.service_generation == owner.generation().value()
            && self.pid == owner.pid().value()
            && self.observed_endpoint == owner.endpoint().as_str()
            && self.serve_window_ms > 0
            && !self.build.is_empty()
            && self.build.len() <= 1_024
    }
}

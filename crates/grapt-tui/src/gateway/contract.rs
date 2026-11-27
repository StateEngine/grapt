// SPDX-License-Identifier: MIT OR Apache-2.0
//! Selector read DTOs copied from StateEngine's public API design and Prime's
//! accepted addition, with its Revision correction.
//! Fixture-only until the API crate ships; never linked by the native binary.
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiFailure {
    NotIncluded,
    Usage,
    Refused,
    Unauthorized,
    Unknown,
    Unreadable,
    Conflict,
    Incomplete,
    Unavailable,
    Cancelled,
    EnginePanicked,
    GenerationPoisoned,
    Internal,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiCommand {
    Tool {
        name: String,
        args: serde_json::Value,
    },
    Manual,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiRemedy {
    pub text: String,
    pub command: Option<ApiCommand>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiRetry {
    Never,
    AfterMillis(u64),
    AfterRefresh,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiError {
    pub kind: ApiFailure,
    pub code: String,
    pub message: String,
    pub remedy: ApiRemedy,
    pub retry: ApiRetry,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectId(pub String);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentId(pub String);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionId(pub String);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreId(pub String);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostSessionId(pub String);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cursor(pub String);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostGeneration(pub u64);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Per-entity counter; creation is one, every committed change adds one.
pub struct Revision(pub std::num::NonZeroU64);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sha256(pub [u8; 32]);
pub type NativePath = PathBuf;
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartIdentity {
    Windows {
        creation_filetime: u64,
    },
    Linux {
        start_ticks: u64,
    },
    Macos {
        start_seconds: u64,
        start_micros: u32,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessInstance {
    pub machine: String,
    pub boot: String,
    pub pid: u32,
    pub started: StartIdentity,
    pub executable: PathBuf,
    pub argv_sha256: Sha256,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostEpoch {
    pub owner: ProcessInstance,
    pub generation: HostGeneration,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    Store {
        store: StoreId,
        project: Option<ProjectId>,
    },
    Host {
        epoch: HostEpoch,
    },
    Configuration {
        location: String,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Read<T> {
    Present {
        value: T,
    },
    Absent {
        checked: String,
    },
    Unreadable {
        source: String,
        reason: String,
        error: Option<ApiError>,
    },
    Unknown {
        what: String,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OmissionReason {
    Limit,
    Budget,
    Unreadable,
    SourceIncomplete,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Omission {
    None,
    Known { count: u64, reason: OmissionReason },
    Unknown { reason: OmissionReason },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    Exhausted,
    ItemLimit,
    ByteLimit,
    Unreadable,
    Cancelled,
    Deadline,
    Unsupported,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Window {
    pub source: Source,
    pub start_cursor: Option<Cursor>,
    pub end_cursor: Option<Cursor>,
    pub returned: u64,
    pub limit: u32,
    pub bytes: u64,
    pub max_bytes: u32,
    pub omitted_before: Omission,
    pub omitted_after: Omission,
    pub unreadable: u64,
    pub stop_reason: StopReason,
    pub complete: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub window: Window,
    pub next: Option<Cursor>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageRequest {
    pub after: Option<Cursor>,
    pub limit: u32,
    pub max_bytes: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionIdentity {
    pub project: ProjectId,
    pub agent: AgentId,
    pub session: SessionId,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectInfo {
    pub id: ProjectId,
    pub name: String,
    pub root: NativePath,
    pub revision: Revision,
}
/// Bounded display metadata; an AgentId is not an offered Grapt agent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentInfo {
    pub project: ProjectId,
    pub id: AgentId,
    pub name: String,
    pub role: String,
    pub revision: Revision,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostSessionTarget {
    pub epoch: HostEpoch,
    pub session: HostSessionId,
    pub binding: SessionIdentity,
    pub binding_fingerprint: Sha256,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionActivity {
    Idle,
    Executing,
    Interrupting,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveSession {
    pub target: HostSessionTarget,
    pub activity: SessionActivity,
}

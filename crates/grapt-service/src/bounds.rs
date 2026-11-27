//! The Rust engine's resource bounds, as **recorded** facts rather than
//! asserted ones.
//!
//! # Why this module exists and why it holds no numbers
//!
//! R4H-H landed a bounded HTTP/1 host whose typed limits are the first
//! measurable resource bounds on the Rust side of the differential. A
//! differential that *asserted* those values would be claiming a property of an
//! engine it did not interrogate — precisely the false-parity move this lane is
//! built to refuse. So the contract requires the driver to **record** each
//! bound from the host's own receipt, and refuses a receipt that is incomplete,
//! duplicated, empty, or sourced from the driver's own opinion.
//!
//! # What this module deliberately does NOT do
//!
//! It does not depend on `grapt-api`, construct an `ApiHostLimits`, re-declare
//! any of its newtypes, or validate a single value. `MaxConnections`
//! refusing zero is `grapt-api`'s law and stays there; nothing here re-states
//! it, and a second validator would be a competing authority on the same fact.
//!
//! The reported value is therefore carried as an **opaque rendering** that this
//! crate never parses, compares numerically, or interprets. That is deliberate:
//! the bounds have several different inner representations upstream
//! (`NonZeroUsize`, `NonZeroU32`, `usize`, `Duration`), so any single scalar
//! this crate chose would be a lossy re-typing — which is exactly how a
//! "recording" quietly becomes a second source of truth.
//!
//! # What binds this list to the real struct
//!
//! A hand-maintained list of names would rot silently the day another bound
//! lands: the driver would under-record and every witness here would stay
//! green, because nothing in this crate can see the other crate's struct. That
//! is why [`HostBoundName::ALL`] is not trusted on its own. The gate in
//! `tests/host_bound_drift.rs` reads `crates/grapt-api/src/limits.rs` and
//! refuses unless this list equals the real field set, in order. This list is a
//! **checked projection** of that struct, which is the ratified direction for a
//! generated-versus-authoritative pair — never a parallel hand-written truth.

use crate::differential::DifferentialRefusal;

/// One of the host's typed resource bounds, named by its exact field.
///
/// Closed on purpose. A tenth bound is a reviewed law change here *and* a red
/// drift gate — not a silently ignored field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HostBoundName {
    /// Live admitted sockets, and the shutdown inventory bound.
    MaxConnections,
    /// Requests admitted on one keep-alive connection.
    MaxRequestsPerConnection,
    /// Requests whose response bodies are live at once.
    MaxInflightRequests,
    /// HTTP event streams, separate from ordinary requests.
    MaxInflightStreams,
    /// The HTTP/1 read-buffer bound.
    MaxRequestHeadBytes,
    /// The parser header count.
    MaxHeaders,
    /// The request-body bound.
    MaxRequestBodyBytes,
    /// The header-read allowance.
    HeaderReadAllowance,
    /// The graceful-drain allowance.
    ShutdownDrainAllowance,
    /// The stream write-stall allowance.
    StreamWriteAllowance,
}

impl HostBoundName {
    /// The exact field name this bound carries in the host's limits struct.
    ///
    /// Fixed text rather than a derived transformation of the variant name.
    /// Deriving it would make the drift gate compare this module against
    /// itself: a renamed variant would rename the expectation in the same
    /// motion, and the comparison would still pass while the recorded corpus
    /// silently changed meaning.
    pub fn field_name(self) -> &'static str {
        match self {
            HostBoundName::MaxConnections => "max_connections",
            HostBoundName::MaxRequestsPerConnection => "max_requests_per_connection",
            HostBoundName::MaxInflightRequests => "max_inflight_requests",
            HostBoundName::MaxInflightStreams => "max_inflight_streams",
            HostBoundName::MaxRequestHeadBytes => "max_request_head_bytes",
            HostBoundName::MaxHeaders => "max_headers",
            HostBoundName::MaxRequestBodyBytes => "max_request_body_bytes",
            HostBoundName::HeaderReadAllowance => "header_read_allowance",
            HostBoundName::ShutdownDrainAllowance => "shutdown_drain_allowance",
            HostBoundName::StreamWriteAllowance => "stream_write_allowance",
        }
    }

    /// The inverse of [`field_name`](Self::field_name). Unknown text is refused
    /// rather than defaulted: a corpus row naming a bound this build does not
    /// know is a fact we cannot interpret, and guessing would attribute a
    /// reading to the wrong bound.
    pub fn from_field_name(name: &str) -> Option<Self> {
        HostBoundName::ALL
            .into_iter()
            .find(|candidate| candidate.field_name() == name)
    }

    /// Every bound the host receipt must report.
    ///
    /// Declaration order is the **recording order** used by
    /// [`RecordedHostBounds`], so a projection is deterministic regardless of
    /// the order a driver happened to read them in.
    pub const ALL: [HostBoundName; 10] = [
        HostBoundName::MaxConnections,
        HostBoundName::MaxRequestsPerConnection,
        HostBoundName::MaxInflightRequests,
        HostBoundName::MaxInflightStreams,
        HostBoundName::MaxRequestHeadBytes,
        HostBoundName::MaxHeaders,
        HostBoundName::MaxRequestBodyBytes,
        HostBoundName::HeaderReadAllowance,
        HostBoundName::ShutdownDrainAllowance,
        HostBoundName::StreamWriteAllowance,
    ];
}

/// Where a recorded bound's value came from.
///
/// The whole point of the enum is that only one variant is admissible. The
/// other two exist so that an inadmissible source has to be *named* in the
/// corpus rather than being unrepresentable — a law nothing can express is a
/// law nothing can witness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoundProvenance {
    /// Read from the host's own receipt. The only admissible source.
    ReportedByHostReceipt,
    /// The driver stated the value itself. Inadmissible: this is the
    /// false-parity move.
    AssertedByDriver,
    /// The driver supplied a default because the host did not report one.
    /// Inadmissible: an unreported bound is a gap, never a default.
    DefaultedByDriver,
}

impl BoundProvenance {
    /// The stable label used in the corpus, fixed independently of the variant
    /// name for the same reason the other labels in this crate are.
    pub fn label(self) -> &'static str {
        match self {
            BoundProvenance::ReportedByHostReceipt => "reported-by-host-receipt",
            BoundProvenance::AssertedByDriver => "asserted-by-driver",
            BoundProvenance::DefaultedByDriver => "defaulted-by-driver",
        }
    }

    /// The inverse of [`label`](Self::label); unknown text is refused, never
    /// defaulted to the admissible variant.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "reported-by-host-receipt" => Some(BoundProvenance::ReportedByHostReceipt),
            "asserted-by-driver" => Some(BoundProvenance::AssertedByDriver),
            "defaulted-by-driver" => Some(BoundProvenance::DefaultedByDriver),
            _ => None,
        }
    }
}

/// One bound as the driver recorded it.
///
/// `reported_rendering` is the host's own rendering of the value, kept verbatim
/// and **never parsed by this crate**. See the module note: interpreting it is
/// how a recording turns into a competing authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedBound {
    name: HostBoundName,
    reported_rendering: String,
    provenance: BoundProvenance,
}

impl RecordedBound {
    pub fn new(
        name: HostBoundName,
        reported_rendering: impl Into<String>,
        provenance: BoundProvenance,
    ) -> Self {
        Self {
            name,
            reported_rendering: reported_rendering.into(),
            provenance,
        }
    }

    pub fn name(&self) -> HostBoundName {
        self.name
    }

    /// The host's verbatim rendering. Opaque by contract.
    pub fn reported_rendering(&self) -> &str {
        &self.reported_rendering
    }

    pub fn provenance(&self) -> BoundProvenance {
        self.provenance
    }
}

/// A complete, host-sourced recording of all nine bounds.
///
/// Constructing one is the only way a differential plan can carry the Rust
/// side's resource bounds, and [`record`](Self::record) is the only
/// constructor — so an incomplete or driver-authored bound set has no path into
/// a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedHostBounds {
    bounds: Vec<RecordedBound>,
}

impl RecordedHostBounds {
    /// Admit a recording, or refuse it.
    ///
    /// Four separate laws, each with its own refusal, because they fail for
    /// different reasons and a single witness for all four would leave three
    /// unproven:
    ///
    /// * every bound is sourced from the host receipt;
    /// * no bound carries an empty rendering — an empty string records nothing
    ///   while looking like a reading;
    /// * no bound is recorded twice, which would let one reading stand in for
    ///   another;
    /// * all nine are present.
    ///
    /// The checks run in that order deliberately. Completeness is reported last
    /// so a set that is *both* incomplete and driver-asserted names the
    /// asserted bound — the more specific defect — rather than reporting only
    /// that something was missing.
    ///
    /// On success the bounds are stored in [`HostBoundName::ALL`] order, so two
    /// drivers that read the same host in different orders produce byte-equal
    /// projections.
    pub fn record(recorded: Vec<RecordedBound>) -> Result<Self, DifferentialRefusal> {
        for bound in &recorded {
            if bound.provenance() != BoundProvenance::ReportedByHostReceipt {
                return Err(DifferentialRefusal::BoundNotReportedByHost {
                    name: bound.name(),
                    provenance: bound.provenance(),
                });
            }
            if bound.reported_rendering().is_empty() {
                return Err(DifferentialRefusal::BoundRenderingEmpty { name: bound.name() });
            }
        }

        let mut ordered = Vec::with_capacity(HostBoundName::ALL.len());
        let mut missing = Vec::new();
        for name in HostBoundName::ALL {
            let mut found = recorded.iter().filter(|bound| bound.name() == name);
            match (found.next(), found.next()) {
                (Some(bound), None) => ordered.push(bound.clone()),
                (Some(_), Some(_)) => return Err(DifferentialRefusal::BoundRecordedTwice { name }),
                (None, _) => missing.push(name),
            }
        }
        if !missing.is_empty() {
            return Err(DifferentialRefusal::BoundSetIncomplete { missing });
        }

        Ok(Self { bounds: ordered })
    }

    /// The nine bounds in [`HostBoundName::ALL`] order.
    pub fn bounds(&self) -> &[RecordedBound] {
        &self.bounds
    }
}

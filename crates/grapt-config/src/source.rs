//! Where a configuration document may come from, and what happened at each of
//! those places.
//!
//! This is **U1b1** of the IA0-2 configuration lane: source custody. It answers
//! one question completely — *which slots exist, in what order, and what was
//! found at each* — and answers no other. The ledger that admits these
//! documents, the ordered contribution that reads them, the provenance each
//! contribution carries, and the bridge that mints `grapt_policy::ConfigLayer`
//! are **U1b2a** and are absent here rather than missing; holding a
//! configuration across loads is **U1b2b** and is absent again. Neither MERGES
//! those documents: a config-owned merge is out of U1b's contract
//! entirely.
//!
//! # The four laws this module exists to hold
//!
//! **Order is farthest-to-nearest.** The census is emitted in ascending
//! precedence: the global slot, then each ancestor from the most distant, then
//! the start directory last. A reader that folds the census front-to-back gets
//! nearest-wins without knowing that is what it is doing.
//!
//! **Rank is derived from the SLOT, never from the result.** A slot's rank is a
//! function of its distance from the start directory and nothing else — not the
//! documents that were found, and not even the bound set the discovery ran
//! under. Every rank is settled before the filesystem is consulted at all.
//! Numbering the documents that happened to be FOUND would mean a file appearing
//! or vanishing silently renumbers its neighbours, and a precedence that moves
//! when an unrelated file is created is not a precedence. This is the law most
//! likely to be "simplified" away, because the simplification passes every test
//! that only ever looks at a fully-populated tree.
//!
//! **Capture is bounded BEFORE it allocates, and again before it completes.**
//! A probe that reports a size over the bound refuses without a read. A file
//! that fits at probe and grows before the read still refuses, because the read
//! itself is capped. One check without the other is a bound with a race in it.
//!
//! **Absent, vanished and unreadable are three states.** A file that was never
//! there, a file that was there and is not, and a file that is there and cannot
//! be read are different facts about the world. Collapsing them into "no
//! document" is how a permission error becomes a silent default.
//!
//! # No ambient input
//!
//! Nothing here reads an environment variable, a command line, or a current
//! working directory. The start directory is supplied by the caller and MUST be
//! absolute; a relative start directory would make the whole ancestor walk a
//! function of an ambient cwd, which is the invention this lane refuses.
//! `GRAPT_CONFIG_CONTENT` and the well-known integration configs remain
//! explicit U1 debt for the completion slice: both need the variable
//! substitution seam, and the first ranks above every slot named here.
//!
//! # This crate's locator is NOT the foundational path identity
//!
//! Law, foundational report row `path`/`hook`/`credential`: "one `PathIdentity`
//! outcome; protected control metadata is removed from writable authority and
//! enforced consistently." `grapt_types::PathIdentity` remains the ONE
//! normalization outcome across every call site, and it is the type that carries
//! authority meaning: a capability's program, cwd, and read/write/deny roots are
//! that type, and a path it cannot resolve is the explicit `Unresolvable`
//! variant rather than a usable path.
//!
//! [`ConfigSourcePath`] is a different thing and must never be mistaken for it.
//! It is a bounded, absolute, purely LEXICAL locator used BEFORE any I/O, whose
//! only job is to name candidate documents so the walk can go and look. It
//! performs no filesystem canonicalization, resolves no link, consults nothing
//! outside the bytes it was handed, and grants no authority whatsoever. It has
//! no unresolvable variant because it never attempts resolution — the states it
//! reports are about what was FOUND at a path ([`SourceOutcome`]), not about
//! what a path resolves to.
//!
//! A consumer that needs an authority answer must CONVERT at the later authority
//! boundary — resolve the located path into `grapt_types::PathIdentity` there —
//! rather than treat this locator as the foundational outcome. Naming this type
//! `PathIdentity` was the defect that made that mistake easy, and the name is
//! gone rather than aliased: an alias would keep the collision reachable while
//! claiming it had been removed.
//!
//! # Paths are secret-bearing
//!
//! A configuration path carries a home directory, a user name and a project
//! name. [`ConfigSourcePath`] therefore renders as a marker under `Debug` and
//! `Display`, on `ConfigKey`'s precedent, and the bytes are reachable only
//! through a named accessor. The census renders slots, ranks and outcome kinds
//! and never a path or a captured byte.

use core::fmt;
use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::bounds::{MaxAncestorDepth, MaxDocumentBytes};
use crate::source_bounds::{MaxPathBytes, SourceBounds};

/// The name of the configuration file this discovery looks for.
///
/// Caller-supplied rather than a constant in this crate. The oracle's
/// well-known names arrive with the integration slice that also owns variable
/// substitution, and inventing them here would put a name in the foundation
/// that the completion slice then has to disagree with.
///
/// A name is one path COMPONENT: no separator, and neither of the two
/// directory-relative names. Admitting `..` would let a file name walk the
/// ancestor chain sideways and land outside every slot the census reports.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfigFileName(String);

impl ConfigFileName {
    /// `None` when the name is empty, contains a separator on any platform, or
    /// is `.` or `..`.
    ///
    /// Both separators are refused on both platforms deliberately. A name
    /// carrying a backslash is a single legal file name on Linux and a two
    /// component path on Windows, and a discovery whose slot count depends on
    /// the host is not deterministic.
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() || raw == "." || raw == ".." {
            return None;
        }
        if raw.contains('/') || raw.contains('\\') {
            return None;
        }
        Some(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Redacted: this name becomes the last component of every path the census
/// reports, and [`ConfigSourcePath`] does not publish those.
impl fmt::Debug for ConfigFileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfigFileName(<redacted>)")
    }
}

/// Why a path was refused as an identity.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathRefusal {
    /// The empty path names nothing.
    #[error("a configuration path must not be empty")]
    Empty,
    /// A relative path would resolve against an ambient working directory.
    #[error("a configuration path must be absolute")]
    NotAbsolute,
    /// Longer than the declared bound, measured on the canonical form.
    #[error("path of {actual} bytes exceeds the bound {bound}")]
    TooLong { actual: usize, bound: usize },
    /// The lexical walk left the root, so the path names nothing reachable.
    #[error("a configuration path must not ascend above its root")]
    AboveRoot,
}

/// A bounded absolute lexical locator for one candidate configuration document.
///
/// # Not `grapt_types::PathIdentity`, and not convertible into one for free
///
/// That type is the single authority/policy/receipt/backend normalization
/// OUTCOME (foundational law row `path`/`hook`/`credential`). This one is a
/// locator used before any I/O: it says where to go and look, grants no
/// authority, and performs no filesystem canonicalization. Converting a located
/// path into the foundational outcome is the later authority boundary's job and
/// is deliberately not offered here, because offering it would let this crate
/// mint an authority answer it has done no work to earn.
///
/// # Lexical, and deliberately not `std::fs::canonicalize`
///
/// Resolving symlinks would put a filesystem round trip inside identity itself:
/// two runs over the same tree could disagree because a link moved between
/// them, and every construction of an identity would become fallible for a
/// reason unrelated to the path. `.` and `..` are folded away, repeated
/// separators collapse, and nothing else is consulted.
///
/// # What this locator does NOT claim
///
/// It is case-SENSITIVE. On Windows two spellings that differ only in case name
/// one file and this type calls them two. That is a filesystem property, not a
/// path property, and a lexical locator that guessed at it would be wrong on
/// every case-sensitive volume — including case-sensitive directories on
/// Windows itself. The honest consequence is stated rather than papered over: a
/// caller that needs case folding must supply already-folded paths.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfigSourcePath(PathBuf);

impl ConfigSourcePath {
    /// Fold `path` lexically and bound the result.
    ///
    /// LEXICAL only: no `std::fs::canonicalize`, no link resolution, no stat.
    /// A path that does not exist yet is admitted, because the whole point of a
    /// locator is to name a place BEFORE finding out what is there. A
    /// constructor that resolved would turn every locator into a filesystem
    /// answer and would quietly become the foundational outcome this type is
    /// separated from.
    ///
    /// The bound is checked on the CANONICAL form, not the input: `a/./././b`
    /// is a long spelling of a short path, and refusing it for its spelling
    /// would make the bound depend on how a caller happened to write it.
    pub fn new(path: &Path, bound: MaxPathBytes) -> Result<Self, PathRefusal> {
        if path.as_os_str().is_empty() {
            return Err(PathRefusal::Empty);
        }
        if !path.is_absolute() {
            return Err(PathRefusal::NotAbsolute);
        }

        let mut canonical = PathBuf::new();
        let mut names = 0usize;
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir => canonical.push(component.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => {
                    if names == 0 {
                        return Err(PathRefusal::AboveRoot);
                    }
                    canonical.pop();
                    names -= 1;
                }
                Component::Normal(name) => {
                    canonical.push(name);
                    names += 1;
                }
            }
        }

        let actual = canonical.as_os_str().len();
        if actual > bound.value() {
            return Err(PathRefusal::TooLong {
                actual,
                bound: bound.value(),
            });
        }
        Ok(Self(canonical))
    }

    /// This path with `name` appended as its final component.
    ///
    /// Re-bounded, because a directory that fits does not imply a file inside it
    /// fits. Re-canonicalising is unnecessary — the receiver is canonical and a
    /// [`ConfigFileName`] cannot be `.`, `..` or a separator — so the join is
    /// the one place this type trusts its own invariant.
    pub fn join_file(
        &self,
        name: &ConfigFileName,
        bound: MaxPathBytes,
    ) -> Result<Self, PathRefusal> {
        let joined = self.0.join(name.as_str());
        let actual = joined.as_os_str().len();
        if actual > bound.value() {
            return Err(PathRefusal::TooLong {
                actual,
                bound: bound.value(),
            });
        }
        Ok(Self(joined))
    }

    /// The directory containing this path, or `None` at the root.
    ///
    /// Already canonical, and a parent of a canonical path is canonical, so no
    /// bound can be exceeded here and none is re-checked.
    pub fn parent(&self) -> Option<Self> {
        self.0.parent().map(|parent| Self(parent.to_path_buf()))
    }

    /// The bytes, for a caller that must actually open the file.
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// A path names a home directory, a user and a project. See the module note.
impl fmt::Debug for ConfigSourcePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfigSourcePath(<redacted>)")
    }
}

impl fmt::Display for ConfigSourcePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// One place a configuration document may sit.
///
/// Closed, and small on purpose. Every slot this crate admits is a function of
/// the caller's plan and the bound set; there is no slot whose existence depends
/// on what is on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceSlot {
    /// The caller's global configuration directory. Weakest, always first.
    Global,
    /// A directory in the ancestor walk. `distance` counts steps ABOVE the
    /// start directory, so `0` is the start directory itself and is strongest.
    Ancestor { distance: usize },
}

/// A slot's precedence, derived from the slot alone.
///
/// Larger wins. `Global` is zero; an ancestor at distance `d` is
/// `MaxAncestorDepth::CEILING - d`, so the start directory takes the ceiling and
/// the most distant admissible ancestor takes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceRank(usize);

impl SourceRank {
    pub fn value(self) -> usize {
        self.0
    }
}

impl SourceSlot {
    /// This slot's rank, or `None` for a distance beyond the deepest walk this
    /// crate admits.
    ///
    /// # Why this takes no bound set
    ///
    /// It is derived from the CRATE's ceiling rather than from the caller's
    /// chosen ancestor depth, so a rank is a property of the slot and of nothing
    /// else. Two consequences, both wanted:
    ///
    /// A document three directories above the start holds the same rank whether
    /// the caller admitted eight ancestors or sixty-four. Deriving it from the
    /// caller's depth would mean the same file in the same tree compared
    /// differently under two bound sets, and a precedence that depends on how
    /// much of the tree you were willing to look at is not a precedence.
    ///
    /// The rank cannot collide, and there is no unrankable-slot fallback for a
    /// caller to trip over: `MaxAncestorDepth` cannot exceed its own ceiling, so
    /// a bounded walk never reaches a distance this refuses. The `None` arm is
    /// therefore total rather than reachable, and [`discover`] REFUSES on it
    /// instead of substituting a rank — substituting one is how two slots come
    /// to share a number, which is exactly the `PrecedenceConflict` that
    /// `grapt_policy::SnapshotBuilder` must never be handed.
    pub fn rank(self) -> Option<SourceRank> {
        match self {
            SourceSlot::Global => Some(SourceRank(0)),
            SourceSlot::Ancestor { distance } => {
                if distance >= MaxAncestorDepth::CEILING {
                    None
                } else {
                    Some(SourceRank(MaxAncestorDepth::CEILING - distance))
                }
            }
        }
    }
}

/// What a reader observed about a path before anything was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceProbe {
    byte_len: usize,
    is_file: bool,
}

impl SourceProbe {
    pub fn new(byte_len: usize, is_file: bool) -> Self {
        Self { byte_len, is_file }
    }

    pub fn byte_len(self) -> usize {
        self.byte_len
    }

    pub fn is_file(self) -> bool {
        self.is_file
    }
}

/// Every way a read of one path can fail, classified.
///
/// Classified from [`io::ErrorKind`] and never from operating-system error text,
/// on `grapt_location`'s precedent: error text is a locale-dependent string
/// that no caller can match on and that may quote the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceIoError {
    /// The path does not exist.
    #[error("no document at this path")]
    NotFound,
    /// The path exists and is not a regular file.
    #[error("this path is not a regular file")]
    NotAFile,
    /// The path exists and cannot be read.
    #[error("this document cannot be read ({kind:?})")]
    Unreadable { kind: io::ErrorKind },
    /// The read reached the cap. The document is larger than the bound admits.
    #[error("this document exceeds the {bound} byte bound")]
    ExceededBound { bound: usize },
}

/// Reads the filesystem on behalf of a discovery.
///
/// Injected rather than called directly so that a discovery witness is a
/// function of a declared tree instead of whatever the machine running the test
/// happens to have. `probe` and `read_bounded` are separate calls because the
/// bound must be applied BETWEEN them — a single `read` entry point could not
/// refuse an oversized document without first having read it.
pub trait SourceReader: fmt::Debug + Send + Sync {
    /// What is at `path`, without reading its contents.
    fn probe(&self, path: &ConfigSourcePath) -> Result<SourceProbe, SourceIoError>;

    /// At most `bound` bytes from `path`.
    ///
    /// An implementation MUST refuse rather than return more than `bound`
    /// bytes, and MUST NOT allocate for more than `bound` bytes: the probe is a
    /// hint about a file that another process may be appending to.
    fn read_bounded(
        &self,
        path: &ConfigSourcePath,
        bound: MaxDocumentBytes,
    ) -> Result<Vec<u8>, SourceIoError>;
}

/// What the discovery found at one slot.
///
/// Closed and exhaustively matched. There is no `Other` arm: a new way for a
/// slot to turn out is a decision somebody makes here, not a state that arrives
/// silently at a caller that was matching on four variants.
#[derive(Clone, PartialEq, Eq)]
pub enum SourceOutcome {
    /// The caller declared no directory for this slot. Distinct from [`Self::Absent`]:
    /// nobody looked, as opposed to somebody looked and found nothing.
    NotConfigured,
    /// The slot exists, was probed, and holds no document.
    Absent,
    /// The document was there at probe time and gone at read time.
    ///
    /// Distinct from [`Self::Absent`] because it is a RACE, not a configuration
    /// state: reporting it as absent would tell a caller the author never wrote
    /// the file.
    Vanished,
    /// The path exists and is not a regular file.
    NotAFile,
    /// The path exists and cannot be read.
    Unreadable { kind: io::ErrorKind },
    /// The probe reported more bytes than the bound admits. NO read was
    /// attempted and nothing was allocated for it.
    TooLargeAtProbe { actual: usize, bound: usize },
    /// The probe fit and the read did not: the document grew in between.
    ///
    /// Distinct from [`Self::TooLargeAtProbe`] because only this one proves the read
    /// cap did any work. If the two were one variant, deleting the read cap
    /// would still produce the surviving variant on the ordinary path and no
    /// witness could tell.
    GrewPastBound { bound: usize },
    /// This exact path was already captured at a stronger slot.
    Duplicate { of: SourceRank },
    /// The document, captured whole and within bound.
    Captured { bytes: Vec<u8> },
}

impl SourceOutcome {
    /// The variant's label, for a diagnostic that must say what happened
    /// without saying what was found.
    pub fn kind(&self) -> &'static str {
        match self {
            SourceOutcome::NotConfigured => "not-configured",
            SourceOutcome::Absent => "absent",
            SourceOutcome::Vanished => "vanished",
            SourceOutcome::NotAFile => "not-a-file",
            SourceOutcome::Unreadable { .. } => "unreadable",
            SourceOutcome::TooLargeAtProbe { .. } => "too-large-at-probe",
            SourceOutcome::GrewPastBound { .. } => "grew-past-bound",
            SourceOutcome::Duplicate { .. } => "duplicate",
            SourceOutcome::Captured { .. } => "captured",
        }
    }

    /// The captured bytes, and `None` for every other outcome.
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            SourceOutcome::Captured { bytes } => Some(bytes),
            _ => None,
        }
    }
}

/// Kind only, and no length. A captured document is the ordinary home of an API
/// key; `crate::document::Redacted` sets the precedent that a length is a weak
/// oracle rather than harmless shape.
impl fmt::Debug for SourceOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceOutcome::Unreadable { kind } => write!(f, "Unreadable({kind:?})"),
            SourceOutcome::TooLargeAtProbe { actual, bound } => {
                write!(f, "TooLargeAtProbe({actual}/{bound})")
            }
            SourceOutcome::GrewPastBound { bound } => write!(f, "GrewPastBound({bound})"),
            SourceOutcome::Duplicate { of } => write!(f, "Duplicate(rank {})", of.value()),
            other => f.write_str(other.kind()),
        }
    }
}

/// One slot, its rank, the path it named, and what was found there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSource {
    slot: SourceSlot,
    rank: SourceRank,
    path: Option<ConfigSourcePath>,
    outcome: SourceOutcome,
}

impl DiscoveredSource {
    pub fn slot(&self) -> SourceSlot {
        self.slot
    }

    pub fn rank(&self) -> SourceRank {
        self.rank
    }

    /// The path probed, and `None` only for a slot the caller did not configure.
    pub fn path(&self) -> Option<&ConfigSourcePath> {
        self.path.as_ref()
    }

    pub fn outcome(&self) -> &SourceOutcome {
        &self.outcome
    }
}

/// Every slot this discovery admitted, in ascending precedence.
///
/// EVERY slot, not every document: a slot that held nothing is reported with
/// [`SourceOutcome::Absent`] and keeps its rank. That is what makes the census a
/// census — a consumer can see that a place was looked at, which a list of found
/// documents cannot express.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCensus {
    entries: Vec<DiscoveredSource>,
}

impl SourceCensus {
    /// Farthest-to-nearest: ascending rank, weakest first.
    pub fn entries(&self) -> &[DiscoveredSource] {
        &self.entries
    }

    /// Only the slots that produced a document, still in ascending precedence.
    pub fn captured(&self) -> impl Iterator<Item = &DiscoveredSource> {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.outcome, SourceOutcome::Captured { .. }))
    }
}

/// What the caller asked to be discovered.
///
/// A plan, not an environment: every path in it was named by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryPlan {
    start: ConfigSourcePath,
    global: Option<ConfigSourcePath>,
    file_name: ConfigFileName,
}

impl DiscoveryPlan {
    pub fn new(
        start: ConfigSourcePath,
        global: Option<ConfigSourcePath>,
        file_name: ConfigFileName,
    ) -> Self {
        Self {
            start,
            global,
            file_name,
        }
    }

    pub fn start(&self) -> &ConfigSourcePath {
        &self.start
    }

    pub fn global(&self) -> Option<&ConfigSourcePath> {
        self.global.as_ref()
    }

    pub fn file_name(&self) -> &ConfigFileName {
        &self.file_name
    }
}

/// Why a discovery could not be performed at all.
///
/// Distinct from a per-slot [`SourceOutcome`]: these are failures of the PLAN,
/// and a plan that cannot be executed must not return a partial census that
/// reads like a complete one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryRefusal {
    /// A slot's path could not be formed within the path bound.
    #[error("a slot path was refused: {refusal}")]
    Path {
        slot: SourceSlot,
        refusal: PathRefusal,
    },
    /// A slot lies beyond the deepest walk this crate ranks.
    ///
    /// Unreachable while the walk is bounded by [`crate::bounds::MaxAncestorDepth`],
    /// which cannot exceed the ceiling the rank uses. It is a REFUSAL rather than
    /// a substituted rank because the alternative to refusing is two slots
    /// sharing one number.
    #[error("a source slot lies beyond the deepest walk this crate ranks")]
    Unrankable { slot: SourceSlot },
}

/// Walk the plan and report every slot.
///
/// The order of the returned census IS the precedence order, ascending. The
/// walk visits the start directory and up to `ancestor_depth - 1` directories
/// above it, stopping early at the filesystem root.
pub fn discover(
    plan: &DiscoveryPlan,
    reader: &dyn SourceReader,
    bounds: SourceBounds,
) -> Result<SourceCensus, DiscoveryRefusal> {
    let mut slots: Vec<(SourceSlot, Option<ConfigSourcePath>)> = Vec::new();

    let global_path = match plan.global() {
        None => None,
        Some(directory) => Some(
            directory
                .join_file(plan.file_name(), bounds.path_bytes())
                .map_err(|refusal| DiscoveryRefusal::Path {
                    slot: SourceSlot::Global,
                    refusal,
                })?,
        ),
    };
    slots.push((SourceSlot::Global, global_path));

    // Farthest first. The walk collects nearest-to-farthest because that is the
    // direction `parent` runs, and is then reversed: emitting in walk order
    // would put the strongest slot first and quietly invert every fold a
    // consumer writes over the census.
    let mut walked: Vec<(SourceSlot, Option<ConfigSourcePath>)> = Vec::new();
    let mut directory = Some(plan.start().clone());
    for distance in 0..bounds.ancestor_depth().value() {
        let Some(current) = directory else {
            break;
        };
        let slot = SourceSlot::Ancestor { distance };
        let path = current
            .join_file(plan.file_name(), bounds.path_bytes())
            .map_err(|refusal| DiscoveryRefusal::Path { slot, refusal })?;
        walked.push((slot, Some(path)));
        directory = current.parent();
    }
    walked.reverse();
    slots.extend(walked);

    // Ranks first, and for every slot, before a single byte is read. A rank is
    // a function of its slot alone, so it can be — and is — settled before the
    // filesystem is consulted at all. That ordering is the law: nothing observed
    // on disk can reach the numbering.
    let mut ranks = Vec::with_capacity(slots.len());
    for (slot, _) in &slots {
        let Some(rank) = slot.rank() else {
            return Err(DiscoveryRefusal::Unrankable { slot: *slot });
        };
        ranks.push(rank);
    }

    // Strongest-first, so a duplicate is reported at the WEAKER slot and the
    // document keeps the precedence it would have had anyway. Walking the
    // census forward instead would let the weak copy win and hand a caller a
    // value from the global slot wearing a project slot's rank.
    let mut claimed: BTreeMap<ConfigSourcePath, SourceRank> = BTreeMap::new();
    let mut outcomes: Vec<Option<SourceOutcome>> = vec![None; slots.len()];
    for index in (0..slots.len()).rev() {
        let (_, path) = &slots[index];
        let Some(path) = path else {
            outcomes[index] = Some(SourceOutcome::NotConfigured);
            continue;
        };
        if let Some(stronger) = claimed.get(path) {
            outcomes[index] = Some(SourceOutcome::Duplicate { of: *stronger });
            continue;
        }
        let outcome = capture(reader, path, bounds.document_bytes());
        if matches!(outcome, SourceOutcome::Captured { .. }) {
            claimed.insert(path.clone(), ranks[index]);
        }
        outcomes[index] = Some(outcome);
    }

    // Annotated rather than inferred from the first push: the element type is
    // what makes a mutation that numbered entries from THIS vector's contents a
    // scoreable red witness instead of an inference error the harness would have
    // to refuse as a compile kill.
    let mut entries: Vec<DiscoveredSource> = Vec::with_capacity(slots.len());
    for (index, (slot, path)) in slots.into_iter().enumerate() {
        let Some(outcome) = outcomes.get_mut(index).and_then(Option::take) else {
            // Every index was filled by the loop above; this arm exists so the
            // function has no panicking path rather than because it can happen.
            continue;
        };
        entries.push(DiscoveredSource {
            slot,
            rank: ranks[index],
            path,
            outcome,
        });
    }
    Ok(SourceCensus { entries })
}

/// Probe, apply the bound, then read under the same bound.
///
/// The probe check is not an optimisation. Without it an oversized document is
/// refused only after the reader has already been asked for it, which is the
/// allocation the bound exists to prevent.
fn capture(
    reader: &dyn SourceReader,
    path: &ConfigSourcePath,
    bound: MaxDocumentBytes,
) -> SourceOutcome {
    let probe = match reader.probe(path) {
        Ok(probe) => probe,
        Err(SourceIoError::NotFound) => return SourceOutcome::Absent,
        Err(SourceIoError::NotAFile) => return SourceOutcome::NotAFile,
        Err(SourceIoError::Unreadable { kind }) => return SourceOutcome::Unreadable { kind },
        Err(SourceIoError::ExceededBound { bound }) => {
            return SourceOutcome::GrewPastBound { bound }
        }
    };
    if !probe.is_file() {
        return SourceOutcome::NotAFile;
    }
    if probe.byte_len() > bound.value() {
        return SourceOutcome::TooLargeAtProbe {
            actual: probe.byte_len(),
            bound: bound.value(),
        };
    }
    match reader.read_bounded(path, bound) {
        Ok(bytes) => SourceOutcome::Captured { bytes },
        // Present at probe, absent at read. A race, not a configuration state.
        Err(SourceIoError::NotFound) => SourceOutcome::Vanished,
        Err(SourceIoError::NotAFile) => SourceOutcome::NotAFile,
        Err(SourceIoError::Unreadable { kind }) => SourceOutcome::Unreadable { kind },
        Err(SourceIoError::ExceededBound { bound }) => SourceOutcome::GrewPastBound { bound },
    }
}

/// The production reader: `std::fs`, bounded.
///
/// Reading a file the CALLER named is not ambient input, which is why this
/// adapter exists at all where `grapt_location` declined to ship a DNS one.
/// What would be ambient — deriving the start directory from the process's
/// working directory, or the global slot from an environment variable — is not
/// here and is not reachable from here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StdSourceReader;

fn classify(error: &io::Error) -> SourceIoError {
    match error.kind() {
        io::ErrorKind::NotFound => SourceIoError::NotFound,
        kind => SourceIoError::Unreadable { kind },
    }
}

impl SourceReader for StdSourceReader {
    fn probe(&self, path: &ConfigSourcePath) -> Result<SourceProbe, SourceIoError> {
        // `metadata` follows links, so a symlink to a regular file probes as
        // the file it names. That is the behaviour an author expects from a
        // configuration path; `ConfigSourcePath` declining to resolve links is
        // about IDENTITY, not about refusing to read through one.
        let metadata = std::fs::metadata(path.as_path()).map_err(|error| classify(&error))?;
        if !metadata.is_file() {
            return Ok(SourceProbe::new(0, false));
        }
        // A length that does not fit in `usize` cannot be captured under any
        // bound this crate admits, so it is reported at the ceiling rather than
        // truncated into something that would pass the bound check.
        let byte_len = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        Ok(SourceProbe::new(byte_len, true))
    }

    fn read_bounded(
        &self,
        path: &ConfigSourcePath,
        bound: MaxDocumentBytes,
    ) -> Result<Vec<u8>, SourceIoError> {
        use std::io::Read;

        let file = std::fs::File::open(path.as_path()).map_err(|error| classify(&error))?;
        // `bound + 1` so an over-bound document is DETECTED rather than
        // silently truncated to a document that happens to parse. The capacity
        // is the bound, never the probed length: the probe is a hint about a
        // file another process may be appending to, and reserving on it is the
        // unbounded allocation this design exists to refuse.
        let mut bytes = Vec::with_capacity(bound.value());
        let read = file
            .take(bound.value() as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| classify(&error))?;
        if read > bound.value() {
            return Err(SourceIoError::ExceededBound {
                bound: bound.value(),
            });
        }
        Ok(bytes)
    }
}

/// A declared filesystem, for witnesses and for fixtures.
///
/// No clock, no ordering surprises and no host: a path either has a declared
/// answer or it is absent. It also counts reads, which is how a witness proves
/// that an over-bound document was refused WITHOUT a read rather than refused
/// after one — a distinction no assertion about the outcome alone can make.
///
/// This is a test authority. It is public because later slices compose it from
/// outside the crate; it is never a production path.
#[derive(Debug, Default)]
pub struct DeterministicSourceReader {
    files: BTreeMap<PathBuf, DeclaredFile>,
    reads: AtomicUsize,
    probes: AtomicUsize,
}

/// What a declared path answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeclaredFile {
    /// Ordinary contents. `probe` reports its length.
    Bytes(Vec<u8>),
    /// A directory: present, and not a regular file.
    Directory,
    /// Present at probe, absent at read. The race, declared.
    VanishesBeforeRead { probed_len: usize },
    /// Probes at `probed_len` and reads longer. The growth, declared.
    GrowsBeforeRead { probed_len: usize, actual: Vec<u8> },
    /// Present and unreadable.
    Unreadable { kind: io::ErrorKind },
}

impl DeterministicSourceReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare ordinary contents at `path`.
    pub fn with_file(mut self, path: &Path, bytes: impl Into<Vec<u8>>) -> Self {
        self.files
            .insert(path.to_path_buf(), DeclaredFile::Bytes(bytes.into()));
        self
    }

    /// Declare a directory at `path`.
    pub fn with_directory(mut self, path: &Path) -> Self {
        self.files
            .insert(path.to_path_buf(), DeclaredFile::Directory);
        self
    }

    /// Declare a document that probes at `probed_len` and is gone by the read.
    pub fn with_vanishing_file(mut self, path: &Path, probed_len: usize) -> Self {
        self.files.insert(
            path.to_path_buf(),
            DeclaredFile::VanishesBeforeRead { probed_len },
        );
        self
    }

    /// Declare a document that probes at `probed_len` and reads `actual`.
    pub fn with_growing_file(
        mut self,
        path: &Path,
        probed_len: usize,
        actual: impl Into<Vec<u8>>,
    ) -> Self {
        self.files.insert(
            path.to_path_buf(),
            DeclaredFile::GrowsBeforeRead {
                probed_len,
                actual: actual.into(),
            },
        );
        self
    }

    /// Declare a present, unreadable document.
    pub fn with_unreadable_file(mut self, path: &Path, kind: io::ErrorKind) -> Self {
        self.files
            .insert(path.to_path_buf(), DeclaredFile::Unreadable { kind });
        self
    }

    /// How many reads this reader has been asked for.
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }

    /// How many probes this reader has been asked for.
    pub fn probes(&self) -> usize {
        self.probes.load(Ordering::Relaxed)
    }
}

impl SourceReader for DeterministicSourceReader {
    fn probe(&self, path: &ConfigSourcePath) -> Result<SourceProbe, SourceIoError> {
        self.probes.fetch_add(1, Ordering::Relaxed);
        match self.files.get(path.as_path()) {
            None => Err(SourceIoError::NotFound),
            Some(DeclaredFile::Bytes(bytes)) => Ok(SourceProbe::new(bytes.len(), true)),
            Some(DeclaredFile::Directory) => Ok(SourceProbe::new(0, false)),
            Some(DeclaredFile::VanishesBeforeRead { probed_len }) => {
                Ok(SourceProbe::new(*probed_len, true))
            }
            Some(DeclaredFile::GrowsBeforeRead { probed_len, .. }) => {
                Ok(SourceProbe::new(*probed_len, true))
            }
            Some(DeclaredFile::Unreadable { kind }) => {
                Err(SourceIoError::Unreadable { kind: *kind })
            }
        }
    }

    fn read_bounded(
        &self,
        path: &ConfigSourcePath,
        bound: MaxDocumentBytes,
    ) -> Result<Vec<u8>, SourceIoError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        match self.files.get(path.as_path()) {
            None => Err(SourceIoError::NotFound),
            Some(DeclaredFile::Bytes(bytes)) => {
                if bytes.len() > bound.value() {
                    Err(SourceIoError::ExceededBound {
                        bound: bound.value(),
                    })
                } else {
                    Ok(bytes.clone())
                }
            }
            Some(DeclaredFile::Directory) => Err(SourceIoError::NotAFile),
            Some(DeclaredFile::VanishesBeforeRead { .. }) => Err(SourceIoError::NotFound),
            Some(DeclaredFile::GrowsBeforeRead { actual, .. }) => {
                if actual.len() > bound.value() {
                    Err(SourceIoError::ExceededBound {
                        bound: bound.value(),
                    })
                } else {
                    Ok(actual.clone())
                }
            }
            Some(DeclaredFile::Unreadable { kind }) => {
                Err(SourceIoError::Unreadable { kind: *kind })
            }
        }
    }
}

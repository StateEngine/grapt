//! A content-addressed filesystem store, write-or-nothing.
//!
//! Why the filesystem and not a SQLite blob: blobs would need the table and
//! migration ownership that F3 holds, so F2B could not be accepted before F3 —
//! and the release order requires exactly that ordering ("F3 starts after F2A
//! and the artifact identity/storage seam it consumes are accepted"). Staying
//! off SQLite also keeps this crate free of any transaction, so nothing here can
//! hold a database guard across an await.
//!
//! ## Identity binds disposition, so the collision cannot be built
//!
//! A pure digest of stored bytes would give a COMPLETE capture of N bytes and a
//! capture TRUNCATED at N bytes of a larger source the same identity: same
//! bytes, different truth. F1A ships `ArtifactRef` and `CaptureDisposition`
//! decoupled, so the reference alone cannot tell them apart.
//!
//! So the two identities do different jobs:
//!
//! * `ArtifactDigest` is the bare lowercase hex SHA-256 of the stored bytes. It
//!   is what a fetch verifies and what an external tool can reproduce.
//! * `ArtifactId` is a digest over a tiny canonical header — domain, version,
//!   disposition, length, and that bytes-digest. Identical bytes with different
//!   dispositions therefore get different IDENTITIES.
//!
//! The reference keeps F1A's frozen `{id, length, digest}` shape, but because
//! the id transitively pins the disposition, a reference that misrepresents
//! whether its bytes are the whole value cannot be constructed to verify. The
//! collision is unrepresentable rather than merely detected.
//!
//! `ArtifactKind` is in that header for the same reason, and the first revision
//! of this crate left it out. Cold review reproduced the consequence: three
//! captures of identical bytes as CapturedOutput, CampaignPlan and Diagnostic
//! collapsed to ONE identity, the later two returned `Ok` with no signal, and a
//! `stat` on the plan reported `CapturedOutput`. An EMPTY capture reaches that
//! collision immediately, and under concurrency the first-writer metadata guard
//! degraded to last-writer, so the reported kind was not even deterministic.
//! Kind is semantic truth a consumer is invited to trust WITHOUT reading the
//! bytes — `ArtifactKind::CampaignPlan` says exactly that — so it belongs in the
//! identity, not in first-writer metadata policy. The header domain is therefore
//! `secode-artifact/id/2`: a structural change to what identity covers has to be
//! a visible version bump, not a silent re-hash.

mod commit;
mod initialization;
mod memory;
#[cfg(test)]
mod metadata_read_tests;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use grapt_types::{ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition, ClockAuthority};

use crate::capture::{drain_bounded, CaptureSink};
use crate::digest::{chunk_size_for, digest_of, RollingDigest};
use crate::store::{
    ArtifactKind, ArtifactMetadata, ArtifactStore, CarryDigestBudget, CarryObservation, FetchError,
    ReadBudget, ReadPage, StatError, Verified,
};
use crate::{CaptureBound, CaptureError, CapturedArtifact, Sensitivity};

/// The hard bound on a metadata record, applied BEFORE the bytes are read.
///
/// A record is a fixed header plus at most `MAX_CHUNKS` hex digest lines, so
/// this is generous by more than an order of magnitude for any honest record and
/// still refuses a corrupt or hostile one before allocation.
const MAX_METADATA_BYTES: u64 = 256 * 1024;

const METADATA_VERSION: &str = "v1";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A content-addressed artifact store, on disk or explicitly in memory.
pub struct FilesystemArtifactStore {
    backend: Backend,
    key: Vec<u8>,
    timing: Option<grapt_types::timing::TimingContext>,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
}

#[derive(Debug, Clone)]
enum Backend {
    Filesystem(Filesystem),
    Memory(Arc<memory::MemoryStore>),
}

#[derive(Debug, Clone)]
struct Filesystem {
    root: PathBuf,
}

impl core::fmt::Debug for FilesystemArtifactStore {
    /// Hand-written: the store key must never reach a log. Printing its length
    /// would leak too, so it is not printed at all.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FilesystemArtifactStore")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl FilesystemArtifactStore {
    /// Open (creating if needed) a store at `root`.
    ///
    /// `key` addresses secret-bearing captures so their identities are not a
    /// stable offline equality oracle. It is never stored, printed or derived
    /// from content. `clock` is injected: no ambient time is read here, so
    /// replay and golden receipts stay deterministic.
    pub fn open(
        root: impl Into<PathBuf>,
        key: impl Into<Vec<u8>>,
        clock: Box<dyn ClockAuthority + Send + Sync>,
    ) -> Result<Self, std::io::Error> {
        let root = root.into();
        initialization::open(&root)?;
        Ok(Self {
            backend: Backend::Filesystem(Filesystem { root }),
            key: key.into(),
            timing: None,
            clock: Arc::from(clock),
        })
    }

    /// Create an isolated volatile store without performing filesystem I/O.
    ///
    /// The store admits at most 64 MiB of retained/capturing payload and 1,024
    /// artifacts, including captures in progress. Each capture reserves its
    /// declared byte bound before reading; insufficient capacity refuses the
    /// capture without eviction or spill to disk. Read buffers are capped at
    /// 64 KiB. Metadata has the same bounded chunk map as the filesystem store.
    ///
    /// References remain valid until the last shared clone is dropped, not
    /// across process restarts. Dropping memory is not secure erasure: allocator,
    /// swap, crash-dump and other OS retention are outside this store's control.
    pub fn in_memory(
        key: impl Into<Vec<u8>>,
        clock: Box<dyn ClockAuthority + Send + Sync>,
    ) -> Self {
        Self {
            backend: Backend::Memory(Arc::new(memory::MemoryStore::default())),
            key: key.into(),
            timing: None,
            clock: Arc::from(clock),
        }
    }

    /// Share the same backing store and clock, without reopening or creating it.
    ///
    /// Memory clones share artifacts, in-flight reservations and capacity.
    pub fn shared_clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            key: self.key.clone(),
            timing: self.timing.clone(),
            clock: Arc::clone(&self.clock),
        }
    }

    pub fn with_timing(mut self, timing: grapt_types::timing::TimingContext) -> Self {
        self.timing = Some(timing);
        self
    }
}

impl Filesystem {
    fn shard(&self, area: &str, id: &str) -> PathBuf {
        let prefix = id.get(..2).unwrap_or("__");
        self.root.join(area).join(prefix)
    }

    fn object_path(&self, id: &str) -> PathBuf {
        self.shard("objects", id).join(id)
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.shard("meta", id).join(id)
    }

    fn temp_path(&self) -> PathBuf {
        let ordinal = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.root
            .join("tmp")
            .join(format!("{}-{ordinal}", std::process::id()))
    }

    /// Write-or-nothing: fill a temp file, fsync it, then rename into place. A
    /// crash mid-write leaves an unreferenced temp file, never a half-written
    /// object that would verify as real.
    fn commit_file(&self, from: &Path, to: &Path) -> Result<(), std::io::Error> {
        commit::publish(from, to, commit::sync_directory)
    }
}

impl FilesystemArtifactStore {
    /// Read-time carry observation: every check of
    /// [`Self::observe_carried`] except the byte re-digest.
    ///
    /// A missing record or object is ABSENT, a record that cannot stand is
    /// UNREADABLE, and a record or object length that contradicts the
    /// reference is CORRUPT. A same-length flip of the object's bytes is NOT
    /// seen here: every byte a reader is served passes through
    /// [`ArtifactStore::fetch`], which digests exactly the page it returns.
    pub fn observe_carried_structure(&self, reference: &ArtifactRef) -> CarryObservation {
        let checked = match &self.backend {
            Backend::Memory(memory) => memory.carried_structure(reference),
            Backend::Filesystem(disk) => disk_structure(disk, reference).map(|_| ()),
        };
        match checked {
            Ok(()) => CarryObservation::Complete,
            Err(observation) => observation,
        }
    }

    /// Admission-time carry observation: does this referenced artifact
    /// stand complete in this store?
    ///
    /// The structural checks of [`Self::observe_carried_structure`] run
    /// first. After the length check the object's actual bytes are
    /// re-digested under the caller's explicit [`CarryDigestBudget`] and
    /// compared with the recorded digest, so a same-length flip is corrupt
    /// at admission; an object that cannot fit the budget is
    /// digest-budget-exceeded. See the type note on [`CarryObservation`].
    pub fn observe_carried(
        &self,
        reference: &ArtifactRef,
        budget: &mut CarryDigestBudget,
    ) -> CarryObservation {
        let disk = match &self.backend {
            Backend::Memory(memory) => return memory.observe_carried(reference, budget, &self.key),
            Backend::Filesystem(disk) => disk,
        };
        let (object_path, meta) = match disk_structure(disk, reference) {
            Ok(checked) => checked,
            Err(observation) => return observation,
        };
        if meta.length > budget.remaining() {
            return CarryObservation::DigestBudgetExceeded;
        }
        match crate::digest::streamed_digest_of(
            &object_path,
            meta.length,
            meta.sensitivity,
            &self.key,
        ) {
            Err(_) => CarryObservation::Unreadable,
            Ok(actual) => {
                budget.charge(meta.length);
                if actual != meta.digest.as_str() {
                    CarryObservation::Corrupt
                } else {
                    CarryObservation::Complete
                }
            }
        }
    }
}

/// The record and object checks a carried reference must pass before any of
/// its bytes are digested: the record path, then the object path and length.
fn disk_structure(
    disk: &Filesystem,
    reference: &ArtifactRef,
) -> Result<(PathBuf, ArtifactMetadata), CarryObservation> {
    let meta_path = disk.meta_path(reference.id().as_str());
    let meta_status = match std::fs::metadata(&meta_path) {
        Ok(status) => status,
        Err(error) => {
            return if error.kind() == std::io::ErrorKind::NotFound {
                Err(CarryObservation::Absent)
            } else {
                Err(CarryObservation::Unreadable)
            };
        }
    };
    if !meta_status.is_file() {
        return Err(CarryObservation::Unreadable);
    }
    let text = match read_text_bounded(&meta_path, MAX_METADATA_BYTES) {
        Ok(text) => text,
        Err(error) => {
            return if error.kind() == std::io::ErrorKind::NotFound {
                Err(CarryObservation::Absent)
            } else {
                Err(CarryObservation::Unreadable)
            };
        }
    };
    let meta = match parse_metadata(&text) {
        Some(meta) => meta,
        None => return Err(CarryObservation::Corrupt),
    };
    if meta.id.as_str() != reference.id().as_str() {
        return Err(CarryObservation::Corrupt);
    }
    let expected = identity_of(
        meta.kind,
        meta.disposition,
        meta.length,
        meta.digest.as_str(),
    );
    if meta.id.as_str() != expected {
        return Err(CarryObservation::Corrupt);
    }
    if reference.length() != meta.length || reference.digest() != &meta.digest {
        return Err(CarryObservation::Corrupt);
    }
    let object_path = disk.object_path(meta.id.as_str());
    let object_status = match std::fs::metadata(&object_path) {
        Ok(status) => status,
        Err(error) => {
            return if error.kind() == std::io::ErrorKind::NotFound {
                Err(CarryObservation::Absent)
            } else {
                Err(CarryObservation::Unreadable)
            };
        }
    };
    if !object_status.is_file() {
        return Err(CarryObservation::Unreadable);
    }
    if object_status.len() != meta.length {
        return Err(CarryObservation::Corrupt);
    }
    Ok((object_path, meta))
}

/// The identity over a tiny canonical header. See the module note: this is what
/// makes disposition AND kind part of the identity.
fn identity_of(
    kind: ArtifactKind,
    disposition: CaptureDisposition,
    length: u64,
    bytes_digest: &str,
) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"secode-artifact/id/2");
    hasher.update(match kind {
        ArtifactKind::CapturedOutput => b"captured-output".as_slice(),
        ArtifactKind::CampaignPlan => b"campaign-plan".as_slice(),
        ArtifactKind::Diagnostic => b"diagnostic".as_slice(),
    });
    hasher.update(match disposition {
        CaptureDisposition::Complete => b"complete".as_slice(),
        CaptureDisposition::TruncatedAtBoundary { .. } => b"truncated".as_slice(),
    });
    hasher.update(length.to_be_bytes());
    hasher.update(bytes_digest.as_bytes());
    let raw = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in raw.iter() {
        use core::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

struct FileSink<'a> {
    file: &'a mut std::fs::File,
    rolling: &'a mut RollingDigest,
}

impl CaptureSink for FileSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), std::io::Error> {
        self.rolling.update(bytes);
        self.file.write_all(bytes)
    }
}

fn render_metadata(meta: &ArtifactMetadata) -> String {
    let mut out = String::new();
    out.push_str(METADATA_VERSION);
    out.push('\n');
    out.push_str(&format!("id {}\n", meta.id.as_str()));
    out.push_str(&format!("length {}\n", meta.length));
    out.push_str(&format!("digest {}\n", meta.digest.as_str()));
    out.push_str(match meta.disposition {
        CaptureDisposition::Complete => "disposition complete\n",
        CaptureDisposition::TruncatedAtBoundary { .. } => "disposition truncated\n",
    });
    out.push_str(match meta.sensitivity {
        Sensitivity::Ordinary => "sensitivity ordinary\n",
        Sensitivity::SecretBearing => "sensitivity secret\n",
    });
    out.push_str(match meta.kind {
        ArtifactKind::CapturedOutput => "kind captured-output\n",
        ArtifactKind::CampaignPlan => "kind campaign-plan\n",
        ArtifactKind::Diagnostic => "kind diagnostic\n",
    });
    out.push_str(&format!("chunk_size {}\n", meta.chunk_size));
    out.push_str(&format!("captured_at {}\n", meta.captured_at.as_millis()));
    for chunk in &meta.chunk_digests {
        out.push_str(&format!("chunk {chunk}\n"));
    }
    out
}

/// Strict decode. Anything unexpected is `Unavailable` rather than a repaired
/// guess: a metadata file that does not parse describes an artifact nobody can
/// vouch for.
fn parse_metadata(text: &str) -> Option<ArtifactMetadata> {
    let mut lines = text.lines();
    if lines.next()? != METADATA_VERSION {
        return None;
    }
    let mut id = None;
    let mut length = None;
    let mut digest = None;
    let mut disposition = None;
    let mut sensitivity = None;
    let mut kind = None;
    let mut chunk_size = None;
    let mut captured_at = None;
    let mut chunk_digests = Vec::new();

    for line in lines {
        let (field, value) = line.split_once(' ')?;
        match field {
            "id" => id = ArtifactId::new(value),
            "length" => length = value.parse::<u64>().ok(),
            "digest" => digest = ArtifactDigest::new(value),
            "disposition" => {
                disposition = match value {
                    "complete" => Some(CaptureDisposition::Complete),
                    // `captured` is filled from the recorded length below; a
                    // truncated capture stored exactly `length` bytes.
                    "truncated" => Some(CaptureDisposition::TruncatedAtBoundary { captured: 0 }),
                    _ => return None,
                }
            }
            "sensitivity" => {
                sensitivity = match value {
                    "ordinary" => Some(Sensitivity::Ordinary),
                    "secret" => Some(Sensitivity::SecretBearing),
                    _ => return None,
                }
            }
            "kind" => {
                kind = match value {
                    "captured-output" => Some(ArtifactKind::CapturedOutput),
                    "campaign-plan" => Some(ArtifactKind::CampaignPlan),
                    "diagnostic" => Some(ArtifactKind::Diagnostic),
                    _ => return None,
                }
            }
            "chunk_size" => chunk_size = value.parse::<u64>().ok(),
            "captured_at" => captured_at = value.parse::<i64>().ok(),
            // Bounded on the READ side too. `MAX_CHUNKS` caps what the writer
            // emits; a corrupt record claiming more is refused rather than
            // accumulated.
            "chunk" => {
                if chunk_digests.len() as u64 >= crate::digest::MAX_CHUNKS {
                    return None;
                }
                chunk_digests.push(value.to_owned());
            }
            _ => return None,
        }
    }

    let length = length?;
    let disposition = match disposition? {
        CaptureDisposition::Complete => CaptureDisposition::Complete,
        CaptureDisposition::TruncatedAtBoundary { .. } => {
            CaptureDisposition::TruncatedAtBoundary { captured: length }
        }
    };
    Some(ArtifactMetadata {
        id: id?,
        length,
        digest: digest?,
        disposition,
        sensitivity: sensitivity?,
        kind: kind?,
        chunk_size: chunk_size?,
        chunk_digests,
        captured_at: grapt_types::Timestamp::from_millis(captured_at?),
    })
}

impl ArtifactStore for FilesystemArtifactStore {
    fn capture<R: std::io::Read>(
        &self,
        mut source: R,
        bound: CaptureBound,
        kind: ArtifactKind,
        sensitivity: Sensitivity,
    ) -> Result<CapturedArtifact, CaptureError> {
        let _capture = self
            .timing
            .as_ref()
            .map(|context| context.span(grapt_types::timing::TimingStage::ArtifactCapture));
        let disk = match &self.backend {
            Backend::Memory(memory) => {
                return memory.capture(self, source, bound, kind, sensitivity);
            }
            Backend::Filesystem(disk) => disk,
        };
        // Chunk size is derived from the BOUND, not from the finished length, so
        // it is known before the first byte and the map is capped for any
        // artifact the bound admits.
        let chunk_size = chunk_size_for(bound.max_bytes());
        let key = sensitivity.is_secret_bearing().then(|| self.key.clone());
        let mut rolling = RollingDigest::new(chunk_size, key);

        let temp = disk.temp_path();
        let mut file = std::fs::File::create(&temp).map_err(CaptureError::Store)?;

        let drained = {
            let mut sink = FileSink {
                file: &mut file,
                rolling: &mut rolling,
            };
            drain_bounded(&mut source, &mut sink, bound).map_err(CaptureError::Source)
        };
        let (length, disposition) = match drained {
            Ok(result) => result,
            Err(error) => {
                let _ = std::fs::remove_file(&temp);
                return Err(error);
            }
        };
        {
            let _sync = self
                .timing
                .as_ref()
                .map(|context| context.span(grapt_types::timing::TimingStage::ArtifactObjectSync));
            file.sync_all().map_err(CaptureError::Store)?;
        }
        drop(file);

        let (bytes_digest, chunk_digests) = rolling.finish();
        let identity = identity_of(kind, disposition, length, &bytes_digest);

        let id = ArtifactId::new(identity.clone()).ok_or(CaptureError::UnusableIdentity)?;
        let digest = ArtifactDigest::new(bytes_digest).ok_or(CaptureError::UnusableIdentity)?;

        disk.commit_file(&temp, &disk.object_path(&identity))
            .map_err(CaptureError::Store)?;

        let meta = ArtifactMetadata {
            id: id.clone(),
            length,
            digest: digest.clone(),
            disposition,
            sensitivity,
            kind,
            chunk_size,
            chunk_digests,
            captured_at: self.clock.now(),
        };

        let meta_path = disk.meta_path(&identity);
        if !meta_path.exists() {
            let meta_temp = disk.temp_path();
            let mut handle = std::fs::File::create(&meta_temp).map_err(CaptureError::Store)?;
            handle
                .write_all(render_metadata(&meta).as_bytes())
                .map_err(CaptureError::Store)?;
            {
                let _sync = self.timing.as_ref().map(|context| {
                    context.span(grapt_types::timing::TimingStage::ArtifactMetadataSync)
                });
                handle.sync_all().map_err(CaptureError::Store)?;
            }
            drop(handle);
            disk.commit_file(&meta_temp, &meta_path)
                .map_err(CaptureError::Store)?;
        } else {
            // An existing metadata file can belong to a concurrent publisher
            // still flushing its rename; do not acknowledge before its parents.
            commit::sync_parents(&meta_path, commit::sync_directory)
                .map_err(CaptureError::Store)?;
        }

        Ok(CapturedArtifact::new(
            ArtifactRef::new(id, length, digest),
            disposition,
        ))
    }

    fn stat(&self, id: &ArtifactId) -> Result<ArtifactMetadata, StatError> {
        // Bounded BEFORE allocation. This is the one path documented as the
        // bounded reference decision, and corruption is inside this crate's
        // threat model, so it must not be the path that reads a file of any
        // size. MAX_CHUNKS bounds what the WRITER emits; nothing bounded what
        // the READER ingested until this check existed.
        let meta = match &self.backend {
            Backend::Memory(memory) => memory.metadata(id).map_err(|_| StatError::Unavailable)?,
            Backend::Filesystem(disk) => {
                let text = read_text_bounded(&disk.meta_path(id.as_str()), MAX_METADATA_BYTES)
                    .map_err(|_| StatError::Unavailable)?;
                parse_metadata(&text).ok_or(StatError::Unavailable)?
            }
        };

        // A record must vouch for itself before `stat` reports a typed fact or
        // mints a reference from it. Without this, corrupt metadata makes
        // `stat().kind()` and `.reference()` wrong on the ZERO-PAYLOAD path,
        // which is exactly the path a caller is told to trust without reading.
        if meta.id.as_str() != id.as_str() {
            return Err(StatError::Unavailable);
        }
        let expected = identity_of(
            meta.kind,
            meta.disposition,
            meta.length,
            meta.digest.as_str(),
        );
        if meta.id.as_str() != expected {
            return Err(StatError::Unavailable);
        }
        Ok(meta)
    }

    fn fetch(
        &self,
        reference: &ArtifactRef,
        offset: u64,
        budget: ReadBudget,
    ) -> Result<ReadPage, FetchError> {
        let meta = self
            .stat(reference.id())
            .map_err(|_| FetchError::Unavailable)?;

        // The reference's length and digest are CLAIMS. Check them against what
        // was recorded before touching a payload byte, and refuse uniformly.
        if reference.length() != meta.length {
            return Err(FetchError::Unavailable);
        }
        if reference.digest() != &meta.digest {
            return Err(FetchError::Unavailable);
        }
        // Identity reproduction is NOT re-checked here, deliberately. `stat`
        // looks the record up BY this reference's id and refuses unless the
        // record's own id equals both that lookup key and the identity
        // recomputed from (kind, disposition, length, digest). So by this line
        // `reference.id() == meta.id == expected` holds unconditionally, and a
        // duplicate check could never fire.
        //
        // The mutation run proved exactly that: once the L3 check existed in
        // `stat`, the old fetch-side copy went GREEN under its own mutation
        // because `stat` refuses first. An unfireable branch cannot be
        // witnessed, and unwitnessed code is a liability rather than defence in
        // depth, so identity is verified in exactly one place.

        if meta.chunk_size == 0 || !offset.is_multiple_of(meta.chunk_size) {
            return Err(FetchError::UnalignedOffset {
                chunk_size: meta.chunk_size,
            });
        }
        if offset > meta.length {
            return Err(FetchError::Unavailable);
        }

        // The budget bounds the PAYLOAD this call returns. It does not claim to
        // bound a serialized wire response: the first revision charged a fixed
        // 256-byte "overhead" constant that was never measured against any real
        // response representation, and a declared-but-unenforced budget is worse
        // than an honest narrower one. See `ReadBudget` for the obligation this
        // hands to the API/feed owner.
        let payload_budget = budget.max_payload_bytes();

        let remaining = meta.length.saturating_sub(offset);
        let whole_fits = remaining <= payload_budget && offset == 0;

        let take = if whole_fits {
            remaining
        } else {
            // Pages are chunk-aligned so each one is independently verifiable.
            if payload_budget < meta.chunk_size {
                return Err(FetchError::BudgetTooSmall {
                    minimum: meta.chunk_size,
                });
            }
            core::cmp::min(remaining, meta.chunk_size)
        };

        let bytes = match &self.backend {
            Backend::Memory(memory) => memory.read_range(reference.id(), offset, take),
            Backend::Filesystem(disk) => {
                read_range(&disk.object_path(reference.id().as_str()), offset, take)
            }
        }
        .map_err(|_| FetchError::Unavailable)?;
        if bytes.len() as u64 != take {
            return Err(FetchError::Unavailable);
        }

        // A whole-artifact digest and a single chunk's digest are the same
        // function of the bytes in front of it, so one helper serves both.
        let actual = digest_of(&bytes, meta.sensitivity, &self.key);

        let verified = if whole_fits && offset == 0 && take == meta.length {
            if actual != meta.digest.as_str() {
                return Err(FetchError::Unavailable);
            }
            Verified::Whole
        } else {
            // Verify THIS page against its own recorded chunk digest. Checking
            // only the whole digest on the final page would hand back every
            // earlier page unverified while looking sealed.
            let index = offset / meta.chunk_size;
            let expected = meta
                .chunk_digests
                .get(index as usize)
                .ok_or(FetchError::Unavailable)?;
            if &actual != expected {
                return Err(FetchError::Unavailable);
            }
            Verified::Chunk { index }
        };

        let end = offset.saturating_add(take);
        Ok(ReadPage {
            bytes,
            offset,
            verified,
            next_offset: (end < meta.length).then_some(end),
        })
    }
}

/// Reads a text file, refusing anything over `limit` BEFORE allocating for it.
///
/// `read_to_string` would size its buffer from the file's own length, which is
/// attacker- or corruption-controlled. This checks the length first and then
/// reads at most `limit + 1` bytes, so a file that grows between the check and
/// the read still cannot exceed the bound.
fn read_text_bounded(path: &Path, limit: u64) -> Result<String, std::io::Error> {
    let file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    read_text_with_length(file, length, limit)
}

fn read_text_with_length(
    reader: impl std::io::Read,
    length: u64,
    limit: u64,
) -> Result<String, std::io::Error> {
    use std::io::Read as _;
    if length > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "metadata record exceeds the configured bound",
        ));
    }
    // Take's generic reader cannot use the file's length hint. Reserve only
    // after checking it so small records avoid repeated tiny reads.
    let capacity = usize::try_from(length)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let mut text = String::with_capacity(capacity);
    let read = reader
        .take(limit.saturating_add(1))
        .read_to_string(&mut text)?;
    if read as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "metadata record exceeds the configured bound",
        ));
    }
    Ok(text)
}

fn read_range(path: &Path, offset: u64, take: u64) -> Result<Vec<u8>, std::io::Error> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0_u8; usize::try_from(take).unwrap_or_default()];
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

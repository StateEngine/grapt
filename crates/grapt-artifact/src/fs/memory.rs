//! Volatile artifact bytes and bounded, write-or-nothing publication.
use super::*;
use std::collections::BTreeMap;
use std::io;
use std::sync::{Mutex, MutexGuard};

const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const MAX_ARTIFACTS: usize = 1024;
const MAX_READ_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct MemoryStore {
    contents: Mutex<Contents>,
}

#[derive(Default)]
struct Contents {
    objects: BTreeMap<ArtifactId, Object>,
    // Includes allocated payload capacity and all in-flight capture bounds.
    used: usize,
    pending: usize,
}

struct Object {
    metadata: ArtifactMetadata,
    bytes: Vec<u8>,
}

impl core::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MemoryStore").finish_non_exhaustive()
    }
}

impl MemoryStore {
    fn lock(&self) -> io::Result<MutexGuard<'_, Contents>> {
        self.contents
            .lock()
            .map_err(|_| io::Error::other("memory artifact store is unavailable"))
    }

    fn reserve(&self, bound: CaptureBound) -> io::Result<Reservation<'_>> {
        let bytes = usize::try_from(bound.max_bytes()).map_err(io::Error::other)?;
        let mut contents = self.lock()?;
        if bytes > MAX_PAYLOAD_BYTES.saturating_sub(contents.used)
            || contents.objects.len() + contents.pending >= MAX_ARTIFACTS
        {
            return Err(io::Error::other("memory artifact store capacity exceeded"));
        }
        contents.used += bytes;
        contents.pending += 1;
        Ok(Reservation { store: self, bytes })
    }

    pub(super) fn capture<R: io::Read>(
        &self,
        owner: &FilesystemArtifactStore,
        mut source: R,
        bound: CaptureBound,
        kind: ArtifactKind,
        sensitivity: Sensitivity,
    ) -> Result<CapturedArtifact, CaptureError> {
        let reservation = self.reserve(bound).map_err(CaptureError::Store)?;
        let chunk_size = chunk_size_for(bound.max_bytes());
        let mut rolling = RollingDigest::new(
            chunk_size,
            sensitivity.is_secret_bearing().then(|| owner.key.clone()),
        );
        let mut bytes = Vec::new();
        let mut sink = MemorySink {
            bytes: &mut bytes,
            rolling: &mut rolling,
            limit: reservation.bytes,
            failed: false,
        };
        let read_bound = CaptureBound::new(
            bound.max_bytes(),
            bound.read_buffer_bytes().min(MAX_READ_BUFFER_BYTES),
        )
        .ok_or(CaptureError::UnusableIdentity)?;
        let result = drain_bounded(&mut source, &mut sink, read_bound);
        let storage_failed = sink.failed;
        let (length, disposition) = result.map_err(|error| {
            if storage_failed {
                CaptureError::Store(error)
            } else {
                CaptureError::Source(error)
            }
        })?;
        let (digest, chunk_digests) = rolling.finish();
        let id = ArtifactId::new(identity_of(kind, disposition, length, &digest))
            .ok_or(CaptureError::UnusableIdentity)?;
        let metadata = ArtifactMetadata {
            id,
            length,
            digest: ArtifactDigest::new(digest).ok_or(CaptureError::UnusableIdentity)?,
            disposition,
            sensitivity,
            kind,
            chunk_size,
            chunk_digests,
            captured_at: owner.clock.now(),
        };
        let captured = CapturedArtifact::new(metadata.reference(), disposition);
        reservation
            .publish(metadata, bytes)
            .map_err(CaptureError::Store)?;
        Ok(captured)
    }

    pub(super) fn metadata(&self, id: &ArtifactId) -> io::Result<ArtifactMetadata> {
        self.lock()?
            .objects
            .get(id)
            .map(|object| object.metadata.clone())
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    pub(super) fn read_range(
        &self,
        id: &ArtifactId,
        offset: u64,
        take: u64,
    ) -> io::Result<Vec<u8>> {
        let contents = self.lock()?;
        let object = contents.objects.get(id).ok_or(io::ErrorKind::NotFound)?;
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        let take = usize::try_from(take).map_err(io::Error::other)?;
        let end = start.checked_add(take).ok_or(io::ErrorKind::InvalidInput)?;
        object
            .bytes
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| io::ErrorKind::UnexpectedEof.into())
    }

    /// Every carried check except the byte re-digest, under one lock.
    pub(super) fn carried_structure(
        &self,
        reference: &ArtifactRef,
    ) -> Result<(), CarryObservation> {
        let contents = self.lock().map_err(|_| CarryObservation::Unreadable)?;
        let object = contents
            .objects
            .get(reference.id())
            .ok_or(CarryObservation::Absent)?;
        structure(object, reference)
    }

    pub(super) fn observe_carried(
        &self,
        reference: &ArtifactRef,
        budget: &mut CarryDigestBudget,
        key: &[u8],
    ) -> CarryObservation {
        let contents = match self.lock() {
            Ok(contents) => contents,
            Err(_) => return CarryObservation::Unreadable,
        };
        let Some(object) = contents.objects.get(reference.id()) else {
            return CarryObservation::Absent;
        };
        if let Err(observation) = structure(object, reference) {
            return observation;
        }
        let meta = &object.metadata;
        if meta.length > budget.remaining() {
            return CarryObservation::DigestBudgetExceeded;
        }
        let actual = digest_of(&object.bytes, meta.sensitivity, key);
        budget.charge(meta.length);
        if actual != meta.digest.as_str() {
            CarryObservation::Corrupt
        } else {
            CarryObservation::Complete
        }
    }
}

/// The record and length claims of one held object against its reference.
fn structure(object: &Object, reference: &ArtifactRef) -> Result<(), CarryObservation> {
    let meta = &object.metadata;
    if meta.id != *reference.id()
        || meta.id.as_str()
            != identity_of(
                meta.kind,
                meta.disposition,
                meta.length,
                meta.digest.as_str(),
            )
        || reference.length() != meta.length
        || reference.digest() != &meta.digest
        || object.bytes.len() as u64 != meta.length
    {
        return Err(CarryObservation::Corrupt);
    }
    Ok(())
}

// Reserve before reading, but never hold the store lock while calling a source.
// Drop rolls back capacity on source failure, failed publication or unwinding.
struct Reservation<'a> {
    store: &'a MemoryStore,
    bytes: usize,
}

impl Reservation<'_> {
    fn publish(mut self, metadata: ArtifactMetadata, bytes: Vec<u8>) -> io::Result<()> {
        let mut contents = self.store.lock()?;
        if bytes.capacity() > self.bytes {
            return Err(io::Error::other(
                "memory capture allocation exceeded reservation",
            ));
        }
        contents.used -= self.bytes;
        contents.pending -= 1;
        self.bytes = 0;
        // As on disk, duplicate content preserves the first capture's metadata.
        let added = match contents.objects.entry(metadata.id.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                let added = bytes.capacity();
                entry.insert(Object { metadata, bytes });
                added
            }
            std::collections::btree_map::Entry::Occupied(_) => 0,
        };
        contents.used += added;
        Ok(())
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.bytes != 0 {
            let mut contents = self
                .store
                .contents
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            contents.used -= self.bytes;
            contents.pending -= 1;
        }
    }
}

struct MemorySink<'a> {
    bytes: &'a mut Vec<u8>,
    rolling: &'a mut RollingDigest,
    limit: usize,
    failed: bool,
}

impl CaptureSink for MemorySink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.failed = true;
        let needed = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|needed| *needed <= self.limit)
            .ok_or_else(|| io::Error::other("memory capture capacity exceeded"))?;
        if needed > self.bytes.capacity() {
            let capacity = needed
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.limit);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(io::Error::other)?;
            if self.bytes.capacity() > self.limit {
                return Err(io::Error::other(
                    "memory capture allocation exceeded reservation",
                ));
            }
        }
        self.bytes.extend_from_slice(bytes);
        self.rolling.update(bytes);
        self.failed = false;
        Ok(())
    }
}

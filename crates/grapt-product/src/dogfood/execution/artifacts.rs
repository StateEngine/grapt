//! Opening the shared durable artifact store for execution and checkpoints.
use super::ExecutionRefusal;
use grapt_artifact::FilesystemArtifactStore;
use grapt_store::SystemClock;
use std::path::Path;

pub(super) fn open_artifacts(
    service_root: &Path,
) -> Result<FilesystemArtifactStore, ExecutionRefusal> {
    FilesystemArtifactStore::open(
        service_root.join("dogfood-artifacts"),
        crate::dogfood::PRODUCTION_ARTIFACT_KEY.to_vec(),
        Box::new(SystemClock),
    )
    .map_err(|error| ExecutionRefusal::Compose(error.to_string()))
}

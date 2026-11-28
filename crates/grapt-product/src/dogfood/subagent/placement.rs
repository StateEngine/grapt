//! Prepare a child checkout before entering synchronous admission.

use std::path::Path;

use grapt_types::{IdAuthority, SessionId};

use super::ServerApplication;

#[derive(Debug)]
pub(super) struct PreparedWorktree {
    parent: SessionId,
    source: String,
    directory: String,
}

impl PreparedWorktree {
    pub(super) async fn prepare(
        application: &ServerApplication,
        parent: &SessionId,
    ) -> Result<Self, String> {
        let source = application
            .get_record(parent)?
            .placement
            .directory()
            .to_owned();
        let root = Path::new(&application.catalog_snapshot().directory).join("worktrees");
        match std::fs::create_dir(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("cannot create child worktree directory: {error}")),
        }
        let metadata = std::fs::symlink_metadata(&root)
            .map_err(|error| format!("cannot inspect child worktree directory: {error}"))?;
        if !metadata.is_dir() || linked(&metadata) {
            return Err("child worktree directory must be a real directory, not a link".into());
        }
        let destination = root.join(format!("child-{}", application.ids.mint().as_str()));
        // The existing helper pins committed HEAD, retains a nested cwd, bounds
        // Git I/O and preserves incomplete checkouts. No store guard is held.
        let directory = grapt_worktree::create(Path::new(&source), &destination).await?;
        Ok(Self {
            parent: parent.clone(),
            source,
            directory: directory
                .to_str()
                .ok_or("child worktree directory is not UTF-8")?
                .to_owned(),
        })
    }

    pub(super) fn directory<'a>(
        &'a self,
        parent: &SessionId,
        source: &str,
    ) -> Result<&'a str, String> {
        if &self.parent != parent || self.source != source {
            return Err("the prepared worktree belongs to another parent or placement".into());
        }
        Ok(&self.directory)
    }
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

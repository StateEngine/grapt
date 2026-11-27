//! Bounded source reads and no-clobber publication. Reports never include contents.
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const FILE_BYTES: usize = 1024 * 1024;
const TOTAL_BYTES: usize = 16 * 1024 * 1024;
const ENTRIES: usize = 2048;

#[derive(Default)]
pub(super) struct Plan {
    rows: Vec<String>,
    writes: Vec<(PathBuf, Vec<u8>)>,
    bytes: usize,
    entries: usize,
    directories: Vec<PathBuf>,
    #[cfg(unix)]
    executable: Vec<PathBuf>,
    instruction_roots: Vec<(PathBuf, PathBuf, bool)>,
}

pub(super) fn exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(format!("cannot inspect {}", super::display(path))),
    }
}

pub(super) fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    if !exists(path)? {
        return Ok(None);
    }
    let parent = super::destination::Parent::open(path, false).map_err(|_| {
        format!(
            "source parent is not a stable regular directory: {}",
            super::display(path)
        )
    })?;
    let file = parent
        .read_file()
        .map_err(|_| format!("cannot acquire regular source {}", super::display(path)))?;
    let metadata = file
        .metadata()
        .map_err(|_| "cannot inspect opened import source")?;
    if !metadata.is_file() || linked(&metadata) {
        return Err(format!(
            "source is not a regular file: {}",
            super::display(path)
        ));
    }
    let mut bytes = Vec::new();
    file.take(FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("cannot read {}", super::display(path)))?;
    if bytes.len() > FILE_BYTES {
        return Err(format!("source exceeds 1 MiB: {}", super::display(path)));
    }
    Ok(Some(bytes))
}

impl Plan {
    pub(super) fn row(&mut self, row: String) -> Result<(), String> {
        self.entries += 1;
        if self.entries > ENTRIES {
            return Err("import exceeds 2048 entries".into());
        }
        self.rows.push(row);
        Ok(())
    }
    pub(super) fn add(&mut self, target: PathBuf, bytes: Vec<u8>) -> Result<(), String> {
        let target = super::graph::normalized(&target)?;
        if target.as_os_str().len() > 4096 {
            return Err("import destination exceeds 4096 bytes".into());
        }
        self.bytes = self.bytes.saturating_add(bytes.len());
        if self.bytes > TOTAL_BYTES {
            return Err("import exceeds 16 MiB".into());
        }
        if self.writes.iter().any(|(path, _)| *path == target) {
            return Ok(());
        }
        if exists(&target)? {
            self.row(format!("kept existing {}", super::display(&target)))?;
        } else {
            self.row(format!("create {}", super::display(&target)))?;
            self.writes.push((target, bytes));
        }
        Ok(())
    }
    pub(super) fn copy_skill(&mut self, source: &Path, target: PathBuf) -> Result<(), String> {
        if exists(&target)? || self.directories.iter().any(|root| target.starts_with(root)) {
            return self.row(format!("kept existing skill {}", super::display(&target)));
        }
        self.directories.push(target.clone());
        for file in scan(source)? {
            let relative = file
                .strip_prefix(source)
                .map_err(|_| "skill source escaped its root")?;
            let bytes = read(&file)?.ok_or("skill source disappeared")?;
            let destination = target.join(relative);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(&file)
                    .map_err(|_| "skill source metadata is unreadable")?
                    .permissions()
                    .mode()
                    & 0o111
                    != 0
                {
                    self.executable.push(destination.clone());
                }
            }
            self.add(destination, bytes)?;
        }
        Ok(())
    }
    pub(super) fn instruction_root(
        &mut self,
        path: PathBuf,
        scope: PathBuf,
        body_only: bool,
    ) -> Result<(), String> {
        if self.writes.iter().any(|(target, _)| *target == path) {
            if self.instruction_roots.len() >= 66 {
                return Err("instruction import exceeds 64 rules and two ambient roots".into());
            }
            self.instruction_roots.push((path, scope, body_only));
        }
        Ok(())
    }
    pub(super) fn finish(
        mut self,
        apply: bool,
        kind: &str,
        source: &str,
    ) -> Result<Vec<String>, String> {
        super::rules::validate(&self.instruction_roots, &self.writes)?;
        for (path, scope, rule) in &self.instruction_roots {
            if !*rule || !super::rules::markdown(path) {
                super::graph::validate(path, scope, *rule, &self.writes)?;
            }
        }
        if apply {
            // Reserve each skill root exclusively before writing any of its files.
            for directory in &self.directories {
                let parent = super::destination::Parent::open(directory, true)
                    .map_err(|_| "cannot acquire skill destination parent")?;
                parent.create_directory().map_err(|_| format!("skill destination appeared or could not be created: {}; nothing overwritten", super::display(directory)))?;
            }
            for (path, bytes) in &self.writes {
                let parent = super::destination::Parent::open(path, true).map_err(|_| {
                    format!(
                        "destination parent changed or is not a stable directory: {}",
                        super::display(path)
                    )
                })?;
                let mut file = parent.create_file().map_err(|_| format!("destination appeared or its parent changed: {}; earlier files may have been imported; nothing overwritten", super::display(path)))?;
                #[cfg(unix)]
                if self.executable.contains(path) {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(fs::Permissions::from_mode(0o700))
                        .map_err(|_| "cannot retain skill executable permission")?;
                }
                file.write_all(bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| {
                        format!(
                            "write failed: {}; inspect the partial import before retrying",
                            super::display(path)
                        )
                    })?;
            }
        }
        self.rows.insert(
            0,
            format!(
                "import {kind} from {source} ({})",
                if apply {
                    "apply"
                } else {
                    "dry run; no files written"
                }
            ),
        );
        if self.rows.len() == 1 {
            self.rows.push("No matching setup found.".into());
        }
        Ok(self.rows)
    }
}

fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub(super) fn scan(root: &Path) -> Result<Vec<PathBuf>, String> {
    if !exists(root)? {
        return Ok(Vec::new());
    }
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut files = Vec::new();
    let mut entries = 0;
    while let Some((path, depth)) = pending.pop() {
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| "import source metadata is unreadable")?;
        if linked(&metadata) {
            return Err(format!("linked source refused: {}", super::display(&path)));
        }
        if metadata.is_file() {
            files.push(path);
            continue;
        }
        if !metadata.is_dir() || depth > 16 {
            return Err("import source is not a bounded regular tree".into());
        }
        for entry in fs::read_dir(path).map_err(|_| "import source directory is unreadable")? {
            entries += 1;
            if entries > ENTRIES {
                return Err("import scan exceeds 2048 entries".into());
            }
            pending.push((
                entry
                    .map_err(|_| "import directory entry is unreadable")?
                    .path(),
                depth + 1,
            ));
        }
    }
    files.sort();
    Ok(files)
}

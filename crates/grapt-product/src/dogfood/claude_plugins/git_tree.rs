//! Decode Git's NUL-delimited inventory before allocating any plugin blobs.
use super::snapshot::{CapturedFile, Snapshot};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Blob {
    pub oid: String,
    pub path: String,
    pub bytes: usize,
    pub executable: bool,
}
pub(super) struct Inventory {
    pub blobs: Vec<Blob>,
    directories: BTreeSet<String>,
}
impl Inventory {
    pub fn parse(output: &[u8]) -> Result<Self, String> {
        if output.len() > 5 * 1024 * 1024 {
            return Err("plugin Git inventory exceeds 5 MiB".into());
        }
        if !output.is_empty() && output.last() != Some(&0) {
            return Err("plugin Git inventory is incomplete".into());
        }
        let mut blobs = Vec::new();
        let mut directories = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut total = 0usize;
        for row in output
            .split(|byte| *byte == 0)
            .filter(|row| !row.is_empty())
        {
            let row = std::str::from_utf8(row).map_err(|_| "plugin Git path is not UTF-8")?;
            let (metadata, path) = row
                .split_once('\t')
                .ok_or("plugin Git inventory has no path")?;
            let mut fields = metadata.split_whitespace();
            let mode = fields.next().ok_or("plugin Git file mode is absent")?;
            if !matches!(mode, "100644" | "100755") || fields.next() != Some("blob") {
                return Err("plugin Git tree contains a link, submodule or special file".into());
            }
            let oid = fields
                .next()
                .ok_or("plugin Git object identity is absent")?;
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("plugin Git object identity is malformed".into());
            }
            let bytes = fields
                .next()
                .ok_or("plugin Git blob size is absent")?
                .parse::<usize>()
                .map_err(|_| "plugin Git blob size is malformed")?;
            total = total
                .checked_add(bytes)
                .ok_or("plugin Git size overflowed")?;
            if fields.next().is_some() || bytes > 16 * 1024 * 1024 || total > 64 * 1024 * 1024 {
                return Err("plugin Git tree exceeds its file or 64 MiB bound".into());
            }
            if path.is_empty()
                || path.len() > 1024
                || path.contains([':', '\\'])
                || path.chars().any(char::is_control)
            {
                return Err("plugin Git resource path is not portable and bounded".into());
            }
            let parts = path.split('/').collect::<Vec<_>>();
            if parts.len() > 33 || parts.iter().any(|part| matches!(*part, "" | "." | "..")) {
                return Err("plugin Git resource path escapes its root or depth bound".into());
            }
            if !paths.insert(path.to_owned()) {
                return Err("plugin Git inventory repeats a path".into());
            }
            for (index, _) in path.match_indices('/') {
                directories.insert(path[..index].to_owned());
            }
            if paths.len().saturating_add(directories.len()) > 4096 {
                return Err("plugin Git tree exceeds 4096 entries".into());
            }
            blobs.push(Blob {
                oid: oid.to_owned(),
                path: path.to_owned(),
                bytes,
                executable: mode == "100755",
            });
        }
        if directories.iter().any(|path| paths.contains(path)) {
            return Err("plugin Git resource is both a file and directory".into());
        }
        Ok(Self { blobs, directories })
    }
    pub fn finish(self, contents: Vec<Vec<u8>>) -> Result<Snapshot, String> {
        if contents.len() != self.blobs.len() {
            return Err("plugin Git acquisition omitted a blob".into());
        }
        let mut files = BTreeMap::new();
        for (blob, bytes) in self.blobs.into_iter().zip(contents) {
            if blob.bytes != bytes.len() {
                return Err("plugin Git blob changed size during acquisition".into());
            }
            files.insert(
                blob.path,
                CapturedFile {
                    bytes,
                    executable: cfg!(unix) && blob.executable,
                },
            );
        }
        Snapshot::from_files(files, self.directories)
    }
}

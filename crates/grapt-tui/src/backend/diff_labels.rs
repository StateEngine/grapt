// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// at bb64b894fd67158be0f874872e6d7bfe717e7368. Modified by Grapt:
// trusted existing project/directory snapshots and cross-host separators.
//! Shorten display headers only when the existing location proves their prefix.
use grapt_client::{
    projection::Mark,
    wire::{FormLocation, ProjectRow},
    Slice,
};

pub(super) fn relative<'a>(
    rows: impl Iterator<Item = &'a mut Slice>,
    project: Option<&ProjectRow>,
    location: Option<&FormLocation>,
) {
    let (Some(project), Some(location)) = (project, location) else {
        return;
    };
    if project.id != location.project {
        return;
    }
    let root = project.worktree.replace('\\', "/");
    let directory = location.directory.replace('\\', "/");
    if root.is_empty() || directory.is_empty() {
        return;
    }
    let root = root.trim_end_matches('/');
    let directory = directory.trim_end_matches('/');
    let root_prefix = format!("{root}/");
    if directory != root && !directory.starts_with(&root_prefix) {
        return;
    }
    for slice in rows.filter(|slice| slice.marks.contains(&Mark::Header)) {
        let mut changed = false;
        let lines = slice
            .text
            .split('\n')
            .enumerate()
            .map(|(index, line)| {
                if slice.marks.get(index) != Some(&Mark::Header) {
                    return line.to_owned();
                }
                let Some((verb, rest)) = line.split_once(' ') else {
                    return line.to_owned();
                };
                if !matches!(verb, "patch" | "apply_patch" | "write" | "edit") {
                    return line.to_owned();
                }
                let Some((path, counts)) = rest.rsplit_once(" · Added ") else {
                    return line.to_owned();
                };
                let normalized = path.replace('\\', "/");
                let path = normalized.strip_prefix(&root_prefix);
                let Some(path) = path.filter(|path| !path.is_empty()) else {
                    return line.to_owned();
                };
                changed = true;
                format!("{verb} {path} · Added {counts}")
            })
            .collect::<Vec<_>>();
        if changed {
            slice.text = lines.join("\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_strip_only_the_verified_project_root() -> Result<(), Box<dyn std::error::Error>> {
        for (root, directory, path, expected) in [
            (
                "/work/.known-root",
                "/work/.known-root/crates",
                "/work/.known-root/crates/a.rs",
                "crates/a.rs",
            ),
            (
                "C:\\work\\.known-root",
                "C:\\work\\.known-root\\crates",
                "C:\\work\\.known-root\\crates\\a.rs",
                "crates/a.rs",
            ),
            (
                "/work/project",
                "/work/project",
                "/work/project-other/a.rs",
                "/work/project-other/a.rs",
            ),
            (
                "/work/project",
                "/work/project/crates",
                "crates/a.rs",
                "crates/a.rs",
            ),
            (
                "/work/project",
                "/work/project",
                ".other-root/crates/a.rs",
                ".other-root/crates/a.rs",
            ),
        ] {
            let project = ProjectRow {
                id: grapt_client::wire::project_id("prj_diff_labels")?,
                worktree: root.into(),
                name: None,
            };
            let location = FormLocation {
                project: project.id.clone(),
                directory: directory.into(),
            };
            let source = format!("apply_patch {path} · Added 1 lines, removed 0 lines");
            let mut row = Slice {
                compaction: None,
                footer: None,
                kind: grapt_client::SliceKind::Tool,
                tool_id: None,
                tool_phase: None,
                tool_summary: None,
                read_detail_start: None,
                ordinal: 0,
                closed: true,
                truncated_bytes: 0,
                text: format!("{source}\n{path}"),
                marks: vec![Mark::Header, Mark::Insert],
            };
            relative(std::iter::once(&mut row), Some(&project), None);
            assert_eq!(row.text, format!("{source}\n{path}"));
            relative(std::iter::once(&mut row), Some(&project), Some(&location));
            assert_eq!(
                row.text,
                format!("apply_patch {expected} · Added 1 lines, removed 0 lines\n{path}")
            );
        }
        Ok(())
    }
}

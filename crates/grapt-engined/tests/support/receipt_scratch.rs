//! Test receipts require a filesystem location outside every repository marker.
//! The governed TMPDIR can itself be under a Git ancestor; never alter that
//! ancestor or weaken the production custody refusal to accommodate a fixture.
//!
//! Every caller takes a drop-guarded, per-call root, so a finished test leaves
//! no fixture scratch behind even when it panics.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "../../../../test-support/tmp_guard.rs"]
pub mod tmp_guard;

static CALLS: AtomicUsize = AtomicUsize::new(0);

pub fn outside_temp_dir() -> tmp_guard::Dir {
    #[cfg(unix)]
    let candidates = [std::env::temp_dir(), "/tmp".into(), "/var/tmp".into()];
    #[cfg(not(unix))]
    let candidates = [std::env::temp_dir(), "C:/Windows/Temp".into()];
    let call = CALLS.fetch_add(1, Ordering::Relaxed);
    let root = select_root(
        &candidates,
        std::process::id(),
        call as u32,
        |path| resolve(path, Path::canonicalize),
        |path| path.try_exists(),
    )
    .unwrap_or_else(|error| panic!("cannot select outside-repository fixture scratch: {error}"));
    tmp_guard::Dir::exact_path(root)
        .unwrap_or_else(|error| panic!("cannot create outside-repository fixture scratch: {error}"))
}

// Tests in one process share the per-process root, so a sibling can create it
// between the two observations below; only a link that resolves to nothing is
// a dangling link, and an ordinary entry is observed again.
fn resolve(
    path: &Path,
    mut canonicalize: impl FnMut(&Path) -> io::Result<PathBuf>,
) -> io::Result<PathBuf> {
    match canonicalize(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A dangling link is not an absent directory we may create.
            match path.symlink_metadata() {
                Ok(entry) if entry.file_type().is_symlink() => {
                    Err(io::Error::other("fixture scratch contains a dangling link"))
                }
                Ok(_) => canonicalize(path),
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => Err(error),
                Err(unreadable) => Err(unreadable),
            }
        }
        result => result,
    }
}

// The caller owns the fixed candidate list and the per-call suffix. Callers
// retain their case suffixes and cleanup; this selector neither creates nor
// deletes a directory.
fn select_root(
    candidates: &[PathBuf],
    process_id: u32,
    call: u32,
    mut canonicalize: impl FnMut(&Path) -> io::Result<PathBuf>,
    mut marker_exists: impl FnMut(&Path) -> io::Result<bool>,
) -> io::Result<PathBuf> {
    for candidate in candidates {
        if !candidate.is_absolute() {
            return Err(io::Error::other(
                "fixture scratch candidate must be absolute",
            ));
        }
        let base = match canonicalize(candidate) {
            Ok(base) => base,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let root = base.join(format!("grapt-engined-fixtures-{process_id}-{call}"));
        let mut repository = false;
        // Check both spellings: aliases must not hide a repository ancestor.
        for ancestor in candidate.ancestors().chain(root.ancestors()) {
            repository |= marker_exists(&ancestor.join(".git"))?;
        }
        if repository {
            continue;
        }
        match canonicalize(&root) {
            Ok(resolved) if resolved != root => {
                return Err(io::Error::other(
                    "fixture scratch process directory escapes its base",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        return Ok(root);
    }
    Err(io::Error::other(
        "all fixture scratch candidates are absent or inside a repository",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> [PathBuf; 3] {
        // Absolute on either host; no filesystem or environment mutation needed.
        let anchor = std::env::temp_dir();
        [
            anchor.join("ambient"),
            anchor.join("tmp"),
            anchor.join("var-tmp"),
        ]
    }

    #[test]
    fn selects_third_candidate_when_first_two_have_repository_ancestors() -> io::Result<()> {
        let paths = candidates();
        let selected = select_root(
            &paths,
            17,
            3,
            |p| Ok(p.into()),
            |p| Ok(p == paths[0].join(".git") || p == paths[1].join(".git")),
        )?;
        assert_eq!(selected, paths[2].join("grapt-engined-fixtures-17-3"));
        Ok(())
    }

    #[test]
    fn prefers_ambient_and_preserves_process_isolation() -> io::Result<()> {
        let paths = candidates();
        for id in [17, 18] {
            let selected = select_root(&paths, id, 5, |p| Ok(p.into()), |_| Ok(false))?;
            assert_eq!(
                selected,
                paths[0].join(format!("grapt-engined-fixtures-{id}-5"))
            );
        }
        Ok(())
    }

    #[test]
    fn refuses_when_every_candidate_has_a_repository_ancestor() {
        let result = select_root(&candidates(), 17, 3, |p| Ok(p.into()), |_| Ok(true));
        assert!(result.is_err(), "repository roots must never be selected");
    }

    #[test]
    fn unreadable_ancestry_fails_instead_of_trying_another_candidate() {
        let result = select_root(
            &candidates(),
            17,
            3,
            |p| Ok(p.into()),
            |_| Err(io::ErrorKind::PermissionDenied.into()),
        );
        assert!(matches!(result, Err(e) if e.kind() == io::ErrorKind::PermissionDenied));
    }

    #[test]
    fn absent_candidate_falls_back_but_unreadable_candidate_fails() -> io::Result<()> {
        let paths = candidates();
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::PermissionDenied] {
            let result = select_root(
                &paths,
                17,
                3,
                |p| {
                    if p == paths[0] {
                        Err(kind.into())
                    } else {
                        Ok(p.into())
                    }
                },
                |_| Ok(false),
            );
            if kind == io::ErrorKind::NotFound {
                assert_eq!(result?, paths[1].join("grapt-engined-fixtures-17-3"));
            } else {
                assert!(matches!(result, Err(e) if e.kind() == kind));
            }
        }
        Ok(())
    }

    #[test]
    fn canonical_repository_ancestor_is_not_hidden_by_an_alias() -> io::Result<()> {
        let paths = candidates();
        let repository = std::env::temp_dir().join("repository");
        let selected = select_root(
            &paths,
            17,
            3,
            |p| {
                if p == paths[0] {
                    Ok(repository.join("alias-target"))
                } else {
                    Ok(p.into())
                }
            },
            |p| Ok(p == repository.join(".git")),
        )?;
        assert_eq!(selected, paths[1].join("grapt-engined-fixtures-17-3"));
        Ok(())
    }

    #[test]
    fn existing_process_directory_cannot_escape_selected_base() {
        let paths = candidates();
        let root = paths[0].join("grapt-engined-fixtures-17-3");
        let result = select_root(
            &paths,
            17,
            3,
            |p| {
                if p == root {
                    Ok(paths[2].clone())
                } else {
                    Ok(p.into())
                }
            },
            |_| Ok(false),
        );
        assert!(
            result.is_err(),
            "a redirected process directory must refuse"
        );
    }

    #[test]
    fn relative_candidate_is_refused_before_filesystem_access() {
        let result = select_root(
            &["relative".into()],
            17,
            3,
            |_| panic!("relative candidate must not reach filesystem"),
            |_| panic!("relative candidate must not reach filesystem"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn absent_process_directory_is_admitted_but_unreadable_is_not() -> io::Result<()> {
        let paths = candidates();
        let root = paths[0].join("grapt-engined-fixtures-17-3");
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::PermissionDenied] {
            let result = select_root(
                &paths,
                17,
                3,
                |p| {
                    if p == root {
                        Err(kind.into())
                    } else {
                        Ok(p.into())
                    }
                },
                |_| Ok(false),
            );
            if kind == io::ErrorKind::NotFound {
                assert_eq!(result?, root);
            } else {
                assert!(matches!(result, Err(e) if e.kind() == kind));
            }
        }
        Ok(())
    }

    #[test]
    fn all_absent_candidates_fail_loudly() {
        let result = select_root(
            &candidates(),
            17,
            3,
            |_| Err(io::ErrorKind::NotFound.into()),
            |_| panic!("absent candidate must not reach ancestry check"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_root_created_between_observations_is_resolved_not_called_a_link() -> io::Result<()> {
        let root = tmp_guard::Dir::exact_path(
            std::env::temp_dir().join(format!("grapt-engined-resolve-race-{}", std::process::id())),
        )?;
        let expected = root.canonicalize();
        let mut first = true;
        // The first observation precedes the sibling's create_dir_all.
        let resolved = resolve(&root, |path| {
            if std::mem::take(&mut first) {
                Err(io::ErrorKind::NotFound.into())
            } else {
                path.canonicalize()
            }
        });
        assert_eq!(resolved?, expected?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_is_still_refused() -> io::Result<()> {
        let link = std::env::temp_dir().join(format!(
            "grapt-engined-resolve-dangling-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(link.with_extension("absent"), &link)?;
        let resolved = resolve(&link, Path::canonicalize);
        let _ = std::fs::remove_file(&link);
        assert!(matches!(resolved, Err(e) if e.to_string().contains("dangling link")));
        Ok(())
    }
}

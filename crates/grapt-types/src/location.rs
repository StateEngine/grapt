//! Placement identities. Law: `PRESERVE` — separate opaque values;
//! `WorkspaceId` is optional placement information, never an empty-string
//! sentinel (optionality lives in `Option<WorkspaceId>` at use sites).
//!
//! `LocationId` is canonicalized from a validated [`LocationRef`]
//! (`SECODE_RUST_FOUNDATIONAL_TYPES_AND_LIFECYCLES_2026-08-07.md` §"Identity
//! and lifecycle inventory"). The oracle's `location.ts:canonicalKey` at the
//! frozen `42e4888cc0` is the measured law, and it is short enough to quote:
//!
//! ```text
//! export function canonicalKey(ref: Ref, platform = process.platform) {
//!   return Ref.make({
//!     directory: platform === "win32" ? AbsolutePath.make(ref.directory.replaceAll("\\", "/")) : ref.directory,
//!     workspaceID: ref.workspaceID,
//!   })
//! }
//! ```
//!
//! Two things follow, and both are enforced below rather than paraphrased:
//!
//! - The separator rewrite is **win32-only**. On POSIX a backslash is an
//!   ordinary filename character, so rewriting it everywhere would make
//!   `/tmp/a\b` and `/tmp/a/b` one Location when the oracle keeps them two.
//! - Nothing else is normalized. In particular the oracle does **not** trim a
//!   trailing separator, so `C:/work` and `C:/work/` are two Locations. Trimming
//!   would also eat the separator that makes a drive root (`E:\`) a root at all.
//!
//! Two refs are the same Location when their canonical directory AND workspace
//! both match, which is what [`LocationRef::canonical_id`] encodes.

crate::id::opaque_id! { ProjectId }
crate::id::opaque_id! { LocationId }
crate::id::opaque_id! { WorkspaceId }

/// Separates the workspace from the directory inside a canonical
/// [`LocationId`]. ASCII US (0x1F) is not legal in either component — see
/// [`LocationRefError`] — so the join is unambiguous and two distinct refs can
/// never canonicalize to one id.
const CANONICAL_SEPARATOR: char = '\u{1f}';

/// Whose placement law applies.
///
/// The oracle's `canonicalKey` takes `platform` as a parameter that merely
/// *defaults* to `process.platform`, and its win32 branch is the only place a
/// separator is rewritten. Carrying the same parameter here keeps both laws
/// provable from either host: a Windows seat can still witness the POSIX rule
/// it would otherwise have to take on trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Windows,
    Posix,
}

impl Platform {
    /// This process's platform — the oracle's `process.platform` default.
    #[must_use]
    pub const fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

/// Why a placement was refused. This crate has no dependencies, so `Display`
/// and `Error` are written out rather than derived — the same way [`IdError`]
/// is.
///
/// [`IdError`]: crate::id::IdError
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationRefError {
    EmptyDirectory,
    /// A relative directory is not a placement: it means something different
    /// depending on who reads it. The oracle's `AbsolutePath` carries the same
    /// law; here it is checked rather than assumed.
    RelativeDirectory {
        directory: String,
    },
    /// Guards the canonical join. Without this, a workspace id containing the
    /// separator could forge another Location's canonical key.
    ReservedSeparator {
        component: &'static str,
    },
}

impl std::fmt::Display for LocationRefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyDirectory => f.write_str("a Location directory must not be empty"),
            Self::RelativeDirectory { directory } => {
                write!(f, "a Location directory must be absolute: {directory:?}")
            }
            Self::ReservedSeparator { component } => {
                write!(
                    f,
                    "{component} must not contain the canonical separator (0x1f)"
                )
            }
        }
    }
}

impl std::error::Error for LocationRefError {}

/// Where a Session's work runs, before canonicalization.
///
/// Construction is checked, so an unvalidated or relative directory cannot
/// become a placement by mistake.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocationRef {
    directory: String,
    workspace: Option<WorkspaceId>,
}

impl LocationRef {
    /// Validate and canonicalize for the host platform.
    ///
    /// See [`LocationRef::on`] — this is that, with the oracle's
    /// `process.platform` default supplied.
    pub fn new(directory: &str, workspace: Option<WorkspaceId>) -> Result<Self, LocationRefError> {
        Self::on(Platform::host(), directory, workspace)
    }

    /// Validate and canonicalize under an explicit [`Platform`].
    ///
    /// Canonicalization is exactly the oracle's: on Windows the directory
    /// separator becomes `/`, and on POSIX the directory is left alone. Nothing
    /// is trimmed — see this module's header for why a trailing separator is
    /// significant.
    ///
    /// # Non-parity
    ///
    /// The absoluteness check is a deliberate strengthening, waived here rather
    /// than measured: the oracle's `AbsolutePath` is `Schema.String` with a
    /// brand and no runtime refinement, so `AbsolutePath.make` accepts a
    /// relative path. A relative directory is not a placement — it names
    /// somewhere different for every reader — and a Session run in the wrong
    /// directory is worse than one refused, so it is checked here instead of
    /// assumed. The check follows Node's `path.isAbsolute` for the platform,
    /// which is the rule the oracle's own callers resolve paths under; it only
    /// ever *refuses* input the oracle would have taken, and never
    /// canonicalizes two refs together that the oracle keeps apart.
    pub fn on(
        platform: Platform,
        directory: &str,
        workspace: Option<WorkspaceId>,
    ) -> Result<Self, LocationRefError> {
        if directory.is_empty() {
            return Err(LocationRefError::EmptyDirectory);
        }
        if directory.contains(CANONICAL_SEPARATOR) {
            return Err(LocationRefError::ReservedSeparator {
                component: "a Location directory",
            });
        }
        if let Some(workspace) = &workspace {
            if workspace.as_str().contains(CANONICAL_SEPARATOR) {
                return Err(LocationRefError::ReservedSeparator {
                    component: "a WorkspaceId",
                });
            }
        }
        let normalized = normalize_directory(platform, directory);
        if !is_absolute(platform, &normalized) {
            return Err(LocationRefError::RelativeDirectory {
                directory: directory.to_owned(),
            });
        }
        Ok(Self {
            directory: normalized,
            workspace,
        })
    }

    /// The canonical directory: `/`-separated on Windows, byte-for-byte as
    /// given on POSIX, and never trimmed on either.
    pub fn directory(&self) -> &str {
        &self.directory
    }

    pub fn workspace(&self) -> Option<&WorkspaceId> {
        self.workspace.as_ref()
    }

    /// The canonical key for this placement. Equal refs produce equal ids, and
    /// — because neither component may contain the separator — distinct refs
    /// never produce the same id.
    pub fn canonical_id(&self) -> LocationId {
        let raw = match &self.workspace {
            Some(workspace) => {
                format!("{}{CANONICAL_SEPARATOR}{}", self.directory, workspace)
            }
            None => self.directory.clone(),
        };
        match LocationId::new(raw) {
            Some(id) => id,
            // `directory` is non-empty by construction, so the id is too.
            None => unreachable!("a validated LocationRef has a non-empty directory"),
        }
    }
}

/// The whole of the oracle's canonicalization: `replaceAll("\\", "/")` on
/// win32, and identity everywhere else.
fn normalize_directory(platform: Platform, directory: &str) -> String {
    match platform {
        Platform::Windows => directory.replace('\\', "/"),
        Platform::Posix => directory.to_owned(),
    }
}

/// Node's `path.isAbsolute` for the platform, read after normalization (so on
/// Windows every separator is already `/`).
///
/// Windows accepts a rooted path (`/x`, and so also the UNC `//server/share`)
/// and a rooted drive path (`C:/x`, including the drive root `C:/` itself). A
/// bare `C:` is drive-*relative* and is refused, exactly as Node refuses it.
fn is_absolute(platform: Platform, normalized: &str) -> bool {
    if normalized.starts_with('/') {
        return true;
    }
    match platform {
        Platform::Posix => false,
        Platform::Windows => {
            let mut bytes = normalized.bytes();
            matches!(
                (bytes.next(), bytes.next(), bytes.next()),
                (Some(drive), Some(b':'), Some(b'/')) if drive.is_ascii_alphabetic()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: &str) -> Option<WorkspaceId> {
        WorkspaceId::new(id)
    }

    #[test]
    fn empty_workspace_is_unrepresentable() {
        assert!(WorkspaceId::new("").is_none());
    }

    fn windows(directory: &str) -> LocationRef {
        match LocationRef::on(Platform::Windows, directory, None) {
            Ok(reference) => reference,
            Err(error) => panic!("{directory:?} must be a Windows placement: {error}"),
        }
    }

    fn posix(directory: &str) -> LocationRef {
        match LocationRef::on(Platform::Posix, directory, None) {
            Ok(reference) => reference,
            Err(error) => panic!("{directory:?} must be a POSIX placement: {error}"),
        }
    }

    #[test]
    fn windows_and_posix_separators_canonicalize_together_on_windows() {
        // Oracle law `location.ts:canonicalKey`, win32 branch: backslashes
        // become forward slashes before two refs are compared.
        let back = windows("C:\\workspace\\repo");
        let forward = windows("C:/workspace/repo");
        assert_eq!(back.directory(), "C:/workspace/repo");
        assert_eq!(back.canonical_id(), forward.canonical_id());
        assert_eq!(back, forward);
    }

    #[test]
    fn a_backslash_is_an_ordinary_posix_filename_character() {
        // The oracle rewrites separators ONLY when platform === "win32".
        // Rewriting them on POSIX would merge a file literally named "a\b"
        // with the directory "a/b" — two different places on disk.
        let escaped = posix("/tmp/a\\b");
        let nested = posix("/tmp/a/b");
        assert_eq!(escaped.directory(), "/tmp/a\\b", "kept byte-for-byte");
        assert_ne!(escaped.canonical_id(), nested.canonical_id());
    }

    #[test]
    fn a_windows_drive_root_is_a_placement() {
        // A drive root's trailing separator is what makes it a root. Trimming
        // it left "E:", which is drive-relative, and the placement was refused
        // outright.
        assert_eq!(windows("E:\\").directory(), "E:/");
        assert_eq!(windows("C:/").directory(), "C:/");
        assert_eq!(
            LocationRef::on(Platform::Windows, "C:", None),
            Err(LocationRefError::RelativeDirectory {
                directory: "C:".to_owned()
            }),
            "a bare drive letter is drive-RELATIVE, as Node's path.isAbsolute has it"
        );
    }

    #[test]
    fn a_trailing_separator_is_not_trimmed_away() {
        // `canonicalKey` normalizes separators and nothing else, so these are
        // two Locations in the oracle and must stay two here.
        assert_eq!(windows("C:/work/").directory(), "C:/work/");
        assert_ne!(
            windows("C:/work/").canonical_id(),
            windows("C:/work").canonical_id()
        );
        assert_ne!(
            posix("/work/").canonical_id(),
            posix("/work").canonical_id()
        );
    }

    #[test]
    fn a_windows_drive_path_is_relative_on_posix() {
        // "C:/x" under POSIX names a directory called "C:" beneath the working
        // directory. It is not a placement there.
        assert_eq!(
            LocationRef::on(Platform::Posix, "C:/x", None),
            Err(LocationRefError::RelativeDirectory {
                directory: "C:/x".to_owned()
            })
        );
        assert_eq!(windows("C:/x").directory(), "C:/x");
    }

    #[test]
    fn a_unc_share_is_a_placement() {
        assert_eq!(windows("\\\\server\\share").directory(), "//server/share");
    }

    #[test]
    fn the_workspace_is_part_of_the_canonical_key() {
        let bare = match LocationRef::new("/srv/app", None) {
            Ok(reference) => reference,
            Err(error) => panic!("must validate: {error}"),
        };
        let scoped = match LocationRef::new("/srv/app", workspace("ws_a")) {
            Ok(reference) => reference,
            Err(error) => panic!("must validate: {error}"),
        };
        assert_ne!(
            bare.canonical_id(),
            scoped.canonical_id(),
            "a workspace-scoped Location is not the bare one"
        );
    }

    #[test]
    fn a_relative_directory_is_not_a_placement() {
        assert_eq!(
            LocationRef::new("rust-secode", None),
            Err(LocationRefError::RelativeDirectory {
                directory: "rust-secode".to_owned()
            })
        );
        assert_eq!(
            LocationRef::new("", None),
            Err(LocationRefError::EmptyDirectory)
        );
    }

    #[test]
    fn the_canonical_separator_cannot_be_smuggled_into_either_component() {
        // Without this guard, workspace "b" under directory "/a\u{1f}b" and
        // workspace-less "/a\u{1f}b" would share one canonical id.
        assert!(matches!(
            LocationRef::new("/a\u{1f}b", None),
            Err(LocationRefError::ReservedSeparator { .. })
        ));
        assert!(matches!(
            LocationRef::new("/a", WorkspaceId::new("b\u{1f}c")),
            Err(LocationRefError::ReservedSeparator { .. })
        ));
    }

    #[test]
    fn the_posix_root_is_a_placement_on_either_platform() {
        assert_eq!(posix("/").directory(), "/");
        assert_eq!(windows("/").directory(), "/");
    }

    #[test]
    fn the_default_platform_is_the_host() {
        // `new` is `on(Platform::host(), ..)`, the same way `canonicalKey`
        // defaults its parameter to `process.platform`.
        let host = match LocationRef::new("/srv/app", None) {
            Ok(reference) => reference,
            Err(error) => panic!("must validate: {error}"),
        };
        let explicit = match LocationRef::on(Platform::host(), "/srv/app", None) {
            Ok(reference) => reference,
            Err(error) => panic!("must validate: {error}"),
        };
        assert_eq!(host, explicit);
    }
}

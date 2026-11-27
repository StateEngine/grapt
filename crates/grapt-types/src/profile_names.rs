//! Profile and project names, current and legacy.
//!
//! Installs made before the product rename keep their files under the old
//! names. Every reader prefers the current name and falls back to the legacy
//! one; every writer writes back to the file that was read, so a legacy
//! profile stays a single set of files. Nothing is moved automatically.

/// Which spelling a profile or project uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spelling {
    /// The current names; also what a new profile is created with.
    Current,
    /// The names from before the rename, still used by existing installs.
    Legacy,
}

impl Spelling {
    /// The JSON configuration file name.
    pub const fn config_json(self) -> &'static str {
        match self {
            Self::Current => "grapt.json",
            Self::Legacy => "secode.json", // grapt-rename: keep
        }
    }

    /// The JSON-with-comments configuration file name.
    pub const fn config_jsonc(self) -> &'static str {
        match self {
            Self::Current => "grapt.jsonc",
            Self::Legacy => "secode.jsonc", // grapt-rename: keep
        }
    }

    /// The directory under the user configuration root that holds profiles.
    pub const fn profiles_dir(self) -> &'static str {
        match self {
            Self::Current => "grapt",
            Self::Legacy => "secode", // grapt-rename: keep
        }
    }

    /// The project-local directory inside a repository.
    pub const fn project_dir(self) -> &'static str {
        match self {
            Self::Current => ".grapt",
            Self::Legacy => ".secode", // grapt-rename: keep
        }
    }
}

/// The spelling to use, given which spellings are present, and the one left
/// unread when both are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub spelling: Spelling,
    pub ignored: Option<Spelling>,
}

/// Prefer the current spelling; fall back to the legacy one only when it is
/// the only one present. With neither present, new files use the current one.
pub const fn choose(current_present: bool, legacy_present: bool) -> Choice {
    match (current_present, legacy_present) {
        (true, true) => Choice {
            spelling: Spelling::Current,
            ignored: Some(Spelling::Legacy),
        },
        (false, true) => Choice {
            spelling: Spelling::Legacy,
            ignored: None,
        },
        _ => Choice {
            spelling: Spelling::Current,
            ignored: None,
        },
    }
}

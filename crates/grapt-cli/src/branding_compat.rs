//! Names retained only at the legacy launch-configuration boundary.

pub(crate) const LEGACY_CONFIG_ENV: &str = "OPENCODE_CONFIG";
/// The selector StateEngine sets on `session provision`, from before the
/// product rename. Accepted after `GRAPT_CONFIG` and before the legacy one.
pub(crate) const PRE_RENAME_CONFIG_ENV: &str = "SECODE_CONFIG"; // grapt-rename: keep

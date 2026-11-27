//! Which configuration spelling a profile directory uses.
//!
//! The names live in `grapt_types::profile_names`; this owns the probe. A
//! profile made before the product rename keeps its legacy files, and every
//! reader and writer in a profile follows the spelling chosen here.

use std::io;
use std::path::Path;

use grapt_types::profile_names::{choose, Choice, Spelling};

/// The current names when either current configuration file is present, the
/// legacy names only when neither is and a legacy one is, and the current
/// names for a profile that has neither. A probe error is returned, never
/// treated as absence.
pub fn profile_spelling(profile: &Path) -> io::Result<Choice> {
    let present = |spelling: Spelling| -> io::Result<bool> {
        for name in [spelling.config_json(), spelling.config_jsonc()] {
            match std::fs::symlink_metadata(profile.join(name)) {
                Ok(_) => return Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(false)
    };
    Ok(choose(
        present(Spelling::Current)?,
        present(Spelling::Legacy)?,
    ))
}

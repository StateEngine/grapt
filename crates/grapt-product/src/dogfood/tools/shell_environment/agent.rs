//! Agent command defaults.
//! Copyright 2025 OpenAI.
//! SPDX-License-Identifier: Apache-2.0
//! Modified by Grapt: explicit user overrides win; CI identifies this harness.
use super::*;

// These affect command children only, never the service or R TUI environment.
// Keep them before explicit set and the final include/credential filters.
// User scripts written before the rename test the legacy CI name; both are
// set for one release.
const LEGACY_CI: &str = "SECODE_CI"; // grapt-rename: keep
const AGENT_ENV: [(&str, &str); 12] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("LANG", "C.UTF-8"),
    ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("GRAPT_CI", "1"),
    (LEGACY_CI, "1"),
    ("CI", "1"),
];

impl Policy {
    // Hooks use their existing user-authority policy and never enter this path.
    pub(super) fn for_agent(mut self, windows: bool) -> Result<Self, String> {
        let overrides = std::mem::take(&mut self.set);
        for (name, value) in AGENT_ENV {
            insert(&mut self.set, name.into(), value.into(), windows)?;
        }
        for (name, value) in overrides {
            insert(&mut self.set, name, value, windows)?;
        }
        Ok(self)
    }
}

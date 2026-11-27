//! Catalog isolation for the product child, split from the carrier to keep
//! it under the module ceiling.

use std::path::Path;
use std::process::Command;

/// The per-root catalog home the product reads through GRAPT_TEST_HOME.
pub(super) const CATALOG_HOME: &str = "catalog-home";

/// Every catalog input the product reads from its environment besides the
/// test home. The child never inherits them, so host configuration cannot
/// reach the product and a namespace without HOME behaves like any other.
const AMBIENT_CATALOG: [&str; 7] = [
    "HOME",
    "USERPROFILE",
    "GRAPT_CONFIG_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "GRAPT_CONFIG",
    "GRAPT_CONFIG_CONTENT",
];

/// Point the product child at this root's catalog home, or at none, and
/// strip every ambient catalog input from the child only.
pub(super) fn isolate_catalog<'c>(
    command: &'c mut Command,
    root: &Path,
    catalog_home: bool,
) -> &'c mut Command {
    // Fixture servers must not inherit the test runner seat's identity.
    for (name, _) in std::env::vars_os() {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        if upper.starts_with("STATEENGINE_")
            || upper.starts_with("SECODE_STATEENGINE_")
            || upper == "SECODE_SESSION_ID"
        {
            command.env_remove(name);
        }
    }
    for name in AMBIENT_CATALOG {
        command.env_remove(name);
    }
    if catalog_home {
        command.env("GRAPT_TEST_HOME", root.join(CATALOG_HOME))
    } else {
        command.env_remove("GRAPT_TEST_HOME")
    }
}

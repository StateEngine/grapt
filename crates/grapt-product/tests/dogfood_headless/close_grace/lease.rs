//! Lease settings for the close-grace journeys.
//!
//! The silent-loss grace is a deployment setting (`close_grace_ms` in
//! `service.conf`). A journey that must wait out a whole grace runs it short so
//! the witness stays inside the per-test budget; the product default is
//! exercised by the unit tests in `grapt-api`.
use std::fs;
use std::path::Path;

/// Rewrite the fixture service config so a silent loss is stopped after `ms`.
pub(crate) fn set_close_grace(root: &Path, ms: u64) {
    let path = root.join("service.conf");
    let config = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("service config read failed: {error}"));
    let mut written = Vec::new();
    let mut replaced = false;
    for line in config.lines() {
        if line.starts_with("close_grace_ms=") {
            written.push(format!("close_grace_ms={ms}"));
            replaced = true;
        } else {
            written.push(line.to_owned());
        }
    }
    if !replaced {
        written.push(format!("close_grace_ms={ms}"));
    }
    let mut body = written.join("\n");
    body.push('\n');
    fs::write(&path, body).unwrap_or_else(|error| panic!("service config write failed: {error}"));
}

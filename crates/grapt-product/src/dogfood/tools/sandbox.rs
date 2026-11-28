//! Prepared shell isolation. Configuration selects policy; only the native
//! child reports what actually enforced it. MSYS degradation needs its own grant.
use super::super::catalog_config::{user_owned, CatalogSources};
use grapt_types::RealizedEnforcement;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod config;
#[cfg(test)]
mod git_metadata_tests;
mod preflight;
pub(crate) use preflight::run as preflight;
pub(super) mod msys;
mod notice;
mod receipt;
pub(in crate::dogfood) use receipt::project as project_receipt;
pub(super) use receipt::Receipt;
#[cfg(target_os = "linux")]
pub(in crate::dogfood) mod posix;
#[cfg(any(unix, windows, test))]
mod scratch;
#[cfg(windows)]
pub(super) mod windows;

fn backend() -> &'static str {
    if cfg!(target_os = "linux") {
        "bwrap"
    } else if cfg!(target_os = "macos") {
        "seatbelt"
    } else {
        "mxc"
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(super) fn unsupported_platform_refusal() -> String {
    if cfg!(target_os = "macos") {
        refusal(
            &degraded("macOS Seatbelt shell containment is not wired into this build"),
            "macOS Seatbelt sandbox unavailable; no command ran; set \"sandbox\": {\"enabled\": false} in user grapt.json to opt out",
        )
    } else {
        refusal(
            &degraded("OS shell containment is unavailable on this platform"),
            "sandbox unavailable; no command ran; set \"sandbox\": {\"enabled\": false} in user grapt.json to opt out",
        )
    }
}

pub(in crate::dogfood) fn windows_enabled(root: &Path, service: &Path) -> Result<bool, String> {
    if !cfg!(windows) {
        return Ok(false);
    }
    let sources = CatalogSources::read(
        root,
        super::super::catalog_config::CatalogEnvironment::read(service)?,
    )?;
    Ok(Plan::read(root, &sources)?.is_some())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::dogfood::tools) struct Plan {
    git_metadata_read_only: bool,
    readable: Vec<PathBuf>,
    writable: Vec<PathBuf>,
    protected: Vec<PathBuf>,
    #[serde(default)]
    denied: Vec<PathBuf>,
    network: bool,
    /// When the host temp directory was omitted from `writable` because it is
    /// an ancestor of the workspace, the planner records it here so the helper
    /// (whose environment is cleared) creates a private per-command scratch
    /// directory inside it and sets `TMPDIR`/`TEMP`/`TMP`.
    #[serde(default)]
    private_temp_parent: Option<PathBuf>,
}

impl Plan {
    pub(super) fn git_metadata_read_only(&self) -> bool {
        self.git_metadata_read_only
    }

    #[cfg(any(target_os = "linux", windows, test))]
    pub(super) fn private_temp_parent(&self) -> Option<&Path> {
        self.private_temp_parent.as_deref()
    }

    pub(super) fn policy(&self) -> grapt_sandbox::Policy {
        grapt_sandbox::Policy {
            readable_roots: self.readable.clone(),
            writable_roots: self.writable.clone(),
            protected_paths: self.protected.clone(),
            denied_paths: self.denied.clone(),
            network: if self.network {
                grapt_sandbox::NetworkAccess::On
            } else {
                grapt_sandbox::NetworkAccess::Off
            },
        }
    }

    /// Policy used when previewing scoped write grants before approval: guards
    /// the Unix per-UID scratch anchor even when `private_temp_parent` is still
    /// active, matching `Plan::filesystem()` so overlapping grants fail before
    /// approval rather than later during `WriteTarget::from_file`.
    pub(super) fn preview_policy(&self) -> grapt_sandbox::Policy {
        let mut policy = self.policy();
        policy.protected_paths = self.protected_paths_with_scratch_anchor();
        policy
    }

    fn protected_paths_with_scratch_anchor(&self) -> Vec<PathBuf> {
        #[cfg(unix)]
        {
            let mut protected = self.protected.clone();
            let temp_parent = self
                .private_temp_parent
                .clone()
                .or_else(|| config::canonical_temp_dir().ok());
            if let Some(parent) = temp_parent {
                let anchor = scratch::unix_scratch_anchor_path(&parent);
                if !protected.iter().any(|existing| existing == &anchor) {
                    protected.push(anchor);
                }
            }
            protected
        }
        #[cfg(not(unix))]
        {
            self.protected.clone()
        }
    }

    pub(super) fn path_count(&self) -> usize {
        self.readable.len()
            + self.writable.len()
            + self.protected.len()
            + self.denied.len()
            + usize::from(self.private_temp_parent.is_some())
    }

    pub(super) fn add_write_target(&mut self, path: &Path) -> Result<(), String> {
        let canonical_target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !self.writable.iter().any(|held| held == path) {
            self.writable.push(path.to_path_buf());
        }
        let temp_parent = match self.private_temp_parent.clone() {
            Some(parent) => parent,
            None => config::canonical_temp_dir()?,
        };
        if config::path_overlaps_scratch(path, &temp_parent)
            || config::path_overlaps_scratch(&canonical_target, &temp_parent)
        {
            self.private_temp_parent = None;
            config::protect_unix_scratch_anchor(&mut self.protected, &temp_parent)?;
        }
        config::check_path_budget(self.path_count(), false)
    }

    pub(super) fn read(root: &Path, sources: &CatalogSources) -> Result<Option<Self>, String> {
        config::read(root, sources)
    }

    pub(super) fn filesystem(&self) -> grapt_types::FilesystemAuthority {
        let identities = |paths: &[PathBuf]| -> Vec<grapt_types::PathIdentity> {
            paths
                .iter()
                .map(|path| grapt_types::PathIdentity::canonical(super::path_text(path)))
                .collect()
        };
        let protected = self.protected_paths_with_scratch_anchor();
        grapt_types::FilesystemAuthority::new(
            identities(&self.readable),
            identities(&self.writable),
            identities(protected.as_slice()),
        )
        .with_read_deny(identities(&self.denied))
    }
    pub(super) fn egress(&self) -> grapt_types::EgressAuthority {
        // Explicit all-protocol/all-host/all-port authority differs from the
        // empty destination set used by network-off. It is never inferred.
        let destinations = if self.network {
            grapt_types::EgressDestination::new("*", "*", 0)
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        grapt_types::EgressAuthority::new(destinations)
    }

    /// This is the intended authority, never a claim that a process ran.
    pub(super) fn intended(&self) -> RealizedEnforcement {
        RealizedEnforcement::PlatformSandbox {
            backend: backend().into(),
        }
    }
}

pub(super) fn degraded(reason: impl Into<String>) -> RealizedEnforcement {
    let mut reason = reason.into();
    while reason.len() > 2048 {
        reason.pop();
    }
    RealizedEnforcement::Degraded {
        backend: backend().into(),
        reason,
    }
}

pub(super) fn value(enforcement: &RealizedEnforcement) -> serde_json::Value {
    match enforcement {
        RealizedEnforcement::HarnessOnly => serde_json::json!({"kind":"harness_only"}),
        RealizedEnforcement::PlatformSandbox { backend } => {
            serde_json::json!({"kind":"platform_sandbox","backend":backend})
        }
        RealizedEnforcement::Degraded { backend, reason } => {
            serde_json::json!({"kind":"degraded","backend":backend,"reason":reason})
        }
    }
}

/// Refusals retain structured enforcement even when no process handle exists.
/// The existing failure artifact is their durable record.
pub(super) fn refusal(enforcement: &RealizedEnforcement, detail: &str) -> String {
    failure(enforcement, detail, Some(false))
}

pub(super) fn failure(
    enforcement: &RealizedEnforcement,
    detail: &str,
    ran: Option<bool>,
) -> String {
    serde_json::json!({"enforcement":value(enforcement),"ran":ran,"error":detail}).to_string()
}
/// Internal interpolation consumes command bytes; the durable artifact keeps
/// their execution receipt. Other tools never use this envelope unwrapping.
pub(in crate::dogfood) fn command_text(text: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "shell receipt is malformed")?;
    if value.get("enforcement").is_none()
        || value.get("ran").and_then(serde_json::Value::as_bool) != Some(true)
    {
        return Err("shell receipt does not record a running command".into());
    }
    value
        .get("output")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "shell receipt has no command output".into())
}

pub(in crate::dogfood) fn receipt(raw: &str) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let enforcement = value.get("enforcement")?;
    match enforcement.get("kind")?.as_str()? {
        "harness_only" => {}
        "platform_sandbox" | "degraded"
            if matches!(
                enforcement.get("backend")?.as_str()?,
                "mxc" | "bwrap" | "seatbelt"
            ) => {}
        _ => return None,
    }
    value.get("ran")?;
    Some(value)
}

#[cfg(test)]
mod tests;

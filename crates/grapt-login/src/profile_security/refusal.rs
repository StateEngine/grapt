//! Safe refusal details: fixed operations and numeric facts, never exception text.
use std::io;

#[derive(Debug)]
struct ProtectionRefusal {
    code: &'static str,
    windows: WindowsFailure,
}
impl std::fmt::Display for ProtectionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "operation={}", self.code)?;
        self.windows.fmt(f)?;
        Ok(())
    }
}
impl std::error::Error for ProtectionRefusal {}

/// A fixed diagnostic code; never includes operating-system exception text.
pub fn failure_code(error: &io::Error) -> &'static str {
    refusal(error).map_or("io", |refusal| refusal.code)
}

/// Redacted protection details suitable for user-facing storage refusals.
/// Missing observations remain unavailable, rather than implying a loaded profile.
pub fn failure_diagnostic(error: &io::Error) -> String {
    if let Some(refusal) = refusal(error) {
        format!("{:?} ({refusal})", error.kind())
    } else {
        format!("{:?} (operation=io)", error.kind())
    }
}

fn refusal(error: &io::Error) -> Option<&ProtectionRefusal> {
    error.get_ref().and_then(|inner| inner.downcast_ref())
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WindowsFailure {
    win32_code: Option<u16>,
    code_source: Option<CodeSource>,
    profile_loaded: Option<bool>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CodeSource {
    Native,
    Hresult,
}

impl std::fmt::Display for WindowsFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.code_source, self.win32_code) {
            (Some(source), Some(code)) => {
                let source = match source {
                    CodeSource::Native => "native",
                    CodeSource::Hresult => "HRESULT",
                };
                write!(f, ", win32={code} ({source})")?;
            }
            _ => f.write_str(", win32=unavailable")?,
        }
        let profile = match self.profile_loaded {
            Some(true) => "loaded",
            Some(false) => "unloaded",
            None => "unavailable",
        };
        write!(f, ", user-profile-hive={profile}")
    }
}

#[cfg(any(windows, test))]
pub(super) fn windows_refusal(code: &str, windows: WindowsFailure) -> io::Error {
    if windows.win32_code.is_some() != windows.code_source.is_some() {
        return io::Error::other("inconsistent Windows protection diagnostic");
    }
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        ProtectionRefusal {
            code: refusal_code(format!("error:{code}").as_bytes()),
            windows,
        },
    )
}

#[cfg(any(windows, test))]
pub(crate) fn child_refusal(output: &[u8]) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        ProtectionRefusal {
            code: refusal_code(output),
            windows: WindowsFailure::default(),
        },
    )
}

#[cfg(any(windows, test))]
fn refusal_code(output: &[u8]) -> &'static str {
    match output {
        b"error:request" => "request",
        b"error:seal" => "seal",
        b"error:open" => "open",
        b"error:type" => "type",
        b"error:owner" => "owner",
        b"error:foreign-write" => "foreign-write",
        b"error:verification" => "verification",
        b"error:acl-read" => "acl-read",
        b"error:acl-write" => "acl-write",
        b"error:operation" => "operation",
        _ => "child-failed",
    }
}

//! Exact operating-system admission for the daemon.
//!
//! The 2026-10-19 platform-parity decision supersedes the earlier ruling 6:
//! Windows, Linux and macOS are supported. Every other name is refused before
//! any socket, lock or registration read. Match exact literals rather than
//! `cfg(unix)`, which would also admit unqualified operating systems.
//! The string decision is independently testable on every supported host.

/// Whether this binary supports the named target operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformSupport {
    /// One of the three supported operating systems.
    Supported,
    /// Anything else, carrying the exact name it was asked about so the
    /// refusal can say which platform it refused rather than only that it
    /// refused something.
    Unsupported {
        /// The exact `std::env::consts::OS` value observed.
        target_os: String,
    },
}

/// The supported target operating systems, named explicitly.
///
/// `serve` and `service` both call this with [`std::env::consts::OS`] before
/// any other work, so an unsupported host is refused before a socket, a lock
/// or a registration read is ever attempted.
pub fn platform_support(target_os: &str) -> PlatformSupport {
    match target_os {
        "windows" | "linux" | "macos" => PlatformSupport::Supported,
        other => PlatformSupport::Unsupported {
            target_os: other.to_owned(),
        },
    }
}

impl PlatformSupport {
    /// The operator-facing refusal text, naming the platform.
    ///
    /// A refusal that does not name what it refused reads, in a log, exactly
    /// like a crash.
    pub fn refusal_text(&self) -> Option<String> {
        match self {
            PlatformSupport::Supported => None,
            PlatformSupport::Unsupported { target_os } => Some(format!(
                "grapt-engined does not support target_os={target_os}; \
                 supported operating systems are windows, linux and macos"
            )),
        }
    }
}

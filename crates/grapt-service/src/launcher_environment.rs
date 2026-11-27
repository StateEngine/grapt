//! A launcher's identity is never authority for a service or its tool children.
//!
//! Uses the same boundary as StateEngine's `withoutStateEngineEnv` in
//! `src/secode-launch-prep.mjs`: session capabilities are explicitly provisioned,
//! never inherited from the launching seat.
use std::ffi::OsStr;
use std::process::Command;

/// Case insensitive on every platform, including non-UTF8 suffixes on Unix.
pub fn is_launcher_identity(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    [
        b"STATEENGINE_".as_slice(),
        b"SECODE_STATEENGINE_".as_slice(),
    ]
    .iter()
    .any(|prefix| {
        bytes
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    }) || bytes.eq_ignore_ascii_case(b"SECODE_SESSION_ID")
}

/// Scrub both ambient names and explicit command overrides without mutating
/// this process's environment or changing ordinary toolchain/home settings.
pub fn scrub(command: &mut Command) {
    let names: Vec<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(command.get_envs().map(|(name, _)| name.to_owned()))
        .filter(|name| is_launcher_identity(name))
        .collect();
    for name in names {
        command.env_remove(name);
    }
}

/// Construct a directly launched service without the launching seat's identity.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    scrub(&mut command);
    command
}

/// Direct server launches cannot safely mutate a multithreaded environment.
/// Refuse before opening user state; supported CLI launchers scrub their child.
pub fn require_unbound() -> Result<(), &'static str> {
    if std::env::vars_os().any(|(name, _)| is_launcher_identity(&name)) {
        return Err("service startup refused: inherited StateEngine identity or session capability; start through `grapt service start` or `grapt code`, or remove launcher identity variables before invoking the server directly");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_names_are_distinct_from_ordinary_environment() {
        for name in [
            "STATEENGINE_AGENT_ID",
            "stateengine_seat_launch_pin",
            "SeCoDe_StAtEeNgInE_LIFECYCLE_ID",
            "secode_session_id",
        ] {
            assert!(is_launcher_identity(OsStr::new(name)), "{name}");
        }
        for name in ["PATH", "HOME", "CARGO_HOME", "BUILD_STATEENGINE_MODE"] {
            assert!(!is_launcher_identity(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn explicit_identity_overrides_are_removed_without_losing_tool_settings() {
        let mut command = Command::new("unused-fixture-program");
        command
            .env("stateengine_agent_id", "identity-sentinel")
            .env("SECODE_STATEENGINE_SEAT_LAUNCH_PIN", "capability-sentinel")
            .env("SECODE_SESSION_ID", "session-sentinel")
            .env("PATH", "tool-path-sentinel")
            .env("HOME", "home-sentinel");
        scrub(&mut command);
        let entries: Vec<_> = command.get_envs().collect();
        assert!(entries
            .iter()
            .filter(|(name, _)| is_launcher_identity(name))
            .all(|(_, value)| value.is_none()));
        assert!(entries.contains(&(OsStr::new("PATH"), Some(OsStr::new("tool-path-sentinel")))));
        assert!(entries.contains(&(OsStr::new("HOME"), Some(OsStr::new("home-sentinel")))));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_suffix_cannot_bypass_identity_prefix() {
        use std::os::unix::ffi::OsStrExt;
        assert!(is_launcher_identity(OsStr::from_bytes(b"STATEENGINE_\xff")));
    }
}

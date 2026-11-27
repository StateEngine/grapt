//! The Windows terminal shim's arguments and its one report line.
//!
//! A ConPTY session's own cleanup job is private to its adapter, so the
//! product cannot ask it whether every descendant has exited. The product executable
//! therefore runs as the pseudoconsole root in shim mode: it starts the real
//! shell on the inherited console inside a job it can query, waits for the
//! shell, confirms job-wide exit within a bound, reports once over the
//! product's named pipe and exits with the shell's status unchanged.
//!
//! The format is plain so both ends stay small: one versioned line, at most
//! [`MAX_REPORT_BYTES`]. A missing, late or malformed report is never read as
//! confirmation.
use std::ffi::{OsStr, OsString};

/// The first argument that selects shim mode. Never a user-facing command.
pub const SHIM_FLAG: &str = "__grapt-terminal-shim";
/// Control byte: stop the shell's job now (cancel, timeout or close).
pub const TERMINATE: u8 = b'T';
/// Control byte: a console Ctrl-C did not end the command in time, so stop the
/// job with the console Ctrl-C exit status instead.
pub const INTERRUPT: u8 = b'I';
/// Windows' exit status for a process ended by Ctrl-C.
pub const CONTROL_C_EXIT: u32 = 0xC000_013A;
/// Upper bound on the report line, including its newline.
pub const MAX_REPORT_BYTES: usize = 16 * 1024;
const VERSION: &str = "grapt-shim-report-v1";
const DIAGNOSTIC_VERSION: &str = "grapt-shim-report-v2";

/// How the shim starts the real shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellLaunch {
    /// `cmd.exe /D /S /C "<command>"`, passed through verbatim.
    Cmd {
        program: OsString,
        command: OsString,
    },
    /// Any other program with ordinary arguments.
    Exec {
        program: OsString,
        arguments: Vec<OsString>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimInvocation {
    pub pipe: OsString,
    pub launch: ShellLaunch,
}

/// The arguments after the executable path.
pub fn encode(pipe: &OsStr, launch: &ShellLaunch) -> Vec<OsString> {
    let mut out = vec![OsString::from(SHIM_FLAG), pipe.to_owned()];
    match launch {
        ShellLaunch::Cmd { program, command } => {
            out.extend([OsString::from("cmd"), program.clone(), command.clone()]);
        }
        ShellLaunch::Exec { program, arguments } => {
            out.extend([OsString::from("exec"), program.clone()]);
            out.extend(arguments.iter().cloned());
        }
    }
    out
}

/// `None` when these arguments do not select shim mode at all.
pub fn decode(args: &[OsString]) -> Option<Result<ShimInvocation, String>> {
    let (flag, rest) = args.split_first()?;
    if flag != SHIM_FLAG {
        return None;
    }
    Some(match rest {
        [pipe, mode, program, command] if mode == "cmd" => Ok(ShimInvocation {
            pipe: pipe.clone(),
            launch: ShellLaunch::Cmd {
                program: program.clone(),
                command: command.clone(),
            },
        }),
        [pipe, mode, program, arguments @ ..] if mode == "exec" => Ok(ShimInvocation {
            pipe: pipe.clone(),
            launch: ShellLaunch::Exec {
                program: program.clone(),
                arguments: arguments.to_vec(),
            },
        }),
        _ => Err("terminal shim arguments are malformed".to_owned()),
    })
}

/// Whether every process in the shell's job was seen to exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanup {
    Confirmed,
    Unconfirmed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimReport {
    /// The shell's exit status, all 32 bits.
    pub exit: u32,
    pub cleanup: Cleanup,
    pub diagnostic: Option<serde_json::Value>,
}

pub fn encode_report(report: &ShimReport) -> Vec<u8> {
    if let Some(diagnostic) = &report.diagnostic {
        let cleanup = match &report.cleanup {
            Cleanup::Confirmed => None,
            Cleanup::Unconfirmed(detail) => Some(detail.clone()),
        };
        let mut value =
            serde_json::json!({"exit":report.exit,"cleanup":cleanup,"diagnostic":diagnostic});
        let mut line = format!("{DIAGNOSTIC_VERSION} {value}");
        if line.len() + 1 > MAX_REPORT_BYTES {
            let cleanup = cleanup.map(|detail| detail.chars().take(512).collect::<String>());
            value = serde_json::json!({"exit":report.exit,"cleanup":cleanup,"diagnostic":{"truncated":true}});
            line = format!("{DIAGNOSTIC_VERSION} {value}");
        }
        line.push('\n');
        return line.into_bytes();
    }
    let mut line = match &report.cleanup {
        Cleanup::Confirmed => format!("{VERSION} {} confirmed", report.exit),
        Cleanup::Unconfirmed(detail) => {
            let detail: String = detail
                .chars()
                .map(|character| {
                    if character.is_control() {
                        ' '
                    } else {
                        character
                    }
                })
                .collect();
            format!("{VERSION} {} unconfirmed {detail}", report.exit)
        }
    };
    // Truncate on a character boundary so the line always parses.
    while line.len() + 1 > MAX_REPORT_BYTES {
        line.pop();
    }
    line.push('\n');
    line.into_bytes()
}

pub fn parse_report(bytes: &[u8]) -> Result<ShimReport, String> {
    let malformed = || "terminal shim report is malformed".to_owned();
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(malformed());
    }
    let line = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.strip_suffix('\n'))
        .ok_or_else(malformed)?;
    if let Some(body) = line.strip_prefix(&format!("{DIAGNOSTIC_VERSION} ")) {
        let value: serde_json::Value = serde_json::from_str(body).map_err(|_| malformed())?;
        let object = value
            .as_object()
            .filter(|object| object.len() == 3)
            .ok_or_else(malformed)?;
        let exit = object
            .get("exit")
            .and_then(serde_json::Value::as_u64)
            .and_then(|exit| u32::try_from(exit).ok())
            .ok_or_else(malformed)?;
        let cleanup = match object.get("cleanup") {
            Some(serde_json::Value::Null) => Cleanup::Confirmed,
            Some(serde_json::Value::String(detail)) if !detail.is_empty() => {
                Cleanup::Unconfirmed(detail.clone())
            }
            _ => return Err(malformed()),
        };
        let diagnostic = object
            .get("diagnostic")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(malformed)?;
        return Ok(ShimReport {
            exit,
            cleanup,
            diagnostic: Some(diagnostic),
        });
    }
    let mut parts = line.splitn(4, ' ');
    if parts.next() != Some(VERSION) {
        return Err(malformed());
    }
    let exit = parts
        .next()
        .and_then(|exit| exit.parse::<u32>().ok())
        .ok_or_else(malformed)?;
    let cleanup = match (parts.next(), parts.next()) {
        (Some("confirmed"), None) => Cleanup::Confirmed,
        (Some("unconfirmed"), Some(detail)) if !detail.is_empty() => {
            Cleanup::Unconfirmed(detail.to_owned())
        }
        _ => return Err(malformed()),
    };
    Ok(ShimReport {
        exit,
        cleanup,
        diagnostic: None,
    })
}

/// What is known so far about one root exit's job-wide cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    Pending,
    Confirmed,
    Unconfirmed(String),
}

/// A shim report as evidence for the root exit the product observed.
pub fn report_evidence(report: &ShimReport, observed_exit: u32) -> Evidence {
    if report.exit != observed_exit {
        return Evidence::Unconfirmed(format!(
            "terminal shim reported exit {} but the terminal exited with {observed_exit}",
            report.exit
        ));
    }
    match &report.cleanup {
        Cleanup::Confirmed => Evidence::Confirmed,
        Cleanup::Unconfirmed(reason) => Evidence::Unconfirmed(reason.clone()),
    }
}

/// `None` while cleanup is still pending inside its bound: the exit is
/// withheld and the terminal keeps its capacity. Otherwise the settled
/// cleanup failure, if any; an expired bound is a failure that stays.
pub fn settle(
    evidence: Evidence,
    expired: bool,
    expiry: impl FnOnce() -> String,
) -> Option<Option<String>> {
    match evidence {
        Evidence::Confirmed => Some(None),
        Evidence::Unconfirmed(reason) => Some(Some(reason)),
        Evidence::Pending => expired.then(|| Some(expiry())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn shim_arguments_round_trip_and_ordinary_arguments_are_not_shim_mode() {
        for launch in [
            ShellLaunch::Cmd {
                program: "C:\\Windows\\System32\\cmd.exe".into(),
                command: "echo \"a b\" & exit /b 3".into(),
            },
            ShellLaunch::Exec {
                program: "C:\\Program Files\\PowerShell\\7\\pwsh.exe".into(),
                arguments: os(&["-NoLogo", "-Command", "Write-Output 'x y'", ""]),
            },
        ] {
            let args = encode(OsStr::new(r"\\.\pipe\grapt-terminal-1-2"), &launch);
            assert_eq!(
                decode(&args),
                Some(Ok(ShimInvocation {
                    pipe: r"\\.\pipe\grapt-terminal-1-2".into(),
                    launch,
                }))
            );
        }
        assert_eq!(decode(&os(&["serve", "--port", "0"])), None);
        assert_eq!(decode(&[]), None);
        assert!(matches!(
            decode(&os(&[SHIM_FLAG, "pipe", "cmd", "cmd.exe"])),
            Some(Err(_))
        ));
        assert!(matches!(
            decode(&os(&[SHIM_FLAG, "pipe", "other", "x"])),
            Some(Err(_))
        ));
    }

    #[test]
    fn reports_round_trip_with_every_exit_bit_and_bounded_detail() -> Result<(), String> {
        for report in [
            ShimReport {
                exit: 0,
                diagnostic: None,
                cleanup: Cleanup::Confirmed,
            },
            ShimReport {
                exit: 0xC000_013A,
                diagnostic: None,
                cleanup: Cleanup::Unconfirmed("1 process still active".into()),
            },
        ] {
            assert_eq!(parse_report(&encode_report(&report))?, report);
        }
        let long = ShimReport {
            exit: u32::MAX,
            diagnostic: None,
            cleanup: Cleanup::Unconfirmed(format!("line\nbreak {}", "é".repeat(5000))),
        };
        let diagnostic = ShimReport {
            exit: 1,
            cleanup: Cleanup::Unconfirmed("CreateProcessW refused the shell".into()),
            diagnostic: Some(serde_json::json!({"call":"CreateProcessW","win32_error":5})),
        };
        assert_eq!(parse_report(&encode_report(&diagnostic))?, diagnostic);
        let oversized = ShimReport {
            diagnostic: Some(serde_json::json!({"detail":"x".repeat(MAX_REPORT_BYTES)})),
            ..diagnostic
        };
        let bounded = encode_report(&oversized);
        assert!(bounded.len() <= MAX_REPORT_BYTES);
        let parsed = parse_report(&bounded)?;
        assert_eq!(parsed.exit, 1);
        assert!(matches!(parsed.cleanup, Cleanup::Unconfirmed(_)));
        assert_eq!(
            parsed.diagnostic,
            Some(serde_json::json!({"truncated":true}))
        );
        let bytes = encode_report(&long);
        assert!(bytes.len() <= MAX_REPORT_BYTES);
        let parsed = parse_report(&bytes)?;
        assert_eq!(parsed.exit, u32::MAX);
        assert!(
            matches!(parsed.cleanup, Cleanup::Unconfirmed(detail) if detail.starts_with("line break"))
        );
        Ok(())
    }

    #[test]
    fn nothing_but_an_exact_confirmed_line_reads_as_confirmation() {
        for bytes in [
            &b""[..],
            b"grapt-shim-report-v1 0 confirmed",
            b"grapt-shim-report-v1 0 confirmed extra\n",
            b"grapt-shim-report-v1 -1 confirmed\n",
            b"grapt-shim-report-v2 0 confirmed\n",
            b"grapt-shim-report-v1 0 unconfirmed \n",
            b"grapt-shim-report-v1 0 CONFIRMED\n",
        ] {
            assert!(parse_report(bytes).is_err(), "{bytes:?}");
        }
        assert!(parse_report(&vec![b'a'; MAX_REPORT_BYTES + 1]).is_err());
        // Neither control byte can be mistaken for report text, or for the other.
        let report = encode_report(&ShimReport {
            exit: 0,
            diagnostic: None,
            cleanup: Cleanup::Confirmed,
        });
        assert!(!report.contains(&TERMINATE) && !report.contains(&INTERRUPT));
        assert_ne!(TERMINATE, INTERRUPT);
        // The forced interrupt ends with the same status pipe-mode Ctrl-C reports.
        assert_eq!(
            i32::from_ne_bytes(CONTROL_C_EXIT.to_ne_bytes()),
            -1_073_741_510
        );
    }

    #[test]
    fn an_outliving_descendant_holds_the_exit_until_confirmation_or_a_recorded_expiry() {
        let expiry = || "1 process still active".to_owned();
        // A grandchild still runs after the shell exits: inside the bound the
        // exit is withheld, so the slot stays reserved.
        assert_eq!(settle(Evidence::Pending, false, expiry), None);
        // The job-wide wait succeeds: cleanup completes.
        assert_eq!(settle(Evidence::Confirmed, false, expiry), Some(None));
        assert_eq!(settle(Evidence::Confirmed, true, expiry), Some(None));
        // The bound expires first: the failure is settled and kept.
        assert_eq!(
            settle(Evidence::Pending, true, expiry),
            Some(Some("1 process still active".into()))
        );
        assert_eq!(
            settle(Evidence::Unconfirmed("closed".into()), false, expiry),
            Some(Some("closed".into()))
        );
    }

    #[test]
    fn a_report_confirms_only_the_exit_it_names() {
        let confirmed = ShimReport {
            exit: 3,
            diagnostic: None,
            cleanup: Cleanup::Confirmed,
        };
        assert_eq!(report_evidence(&confirmed, 3), Evidence::Confirmed);
        assert!(matches!(
            report_evidence(&confirmed, 4),
            Evidence::Unconfirmed(_)
        ));
        let unconfirmed = ShimReport {
            exit: 3,
            diagnostic: None,
            cleanup: Cleanup::Unconfirmed("late".into()),
        };
        assert_eq!(
            report_evidence(&unconfirmed, 3),
            Evidence::Unconfirmed("late".into())
        );
    }
}

#[path = "contracts/connection_diagnostics.rs"]
mod connection_diagnostics;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use grapt_cli::{
    AdvisoryError, AdvisoryEvent, BoundedOutput, CancellationCause, CliCommand, CliLimitValues,
    CliLimits, CliLimitsError, ClientEndpoint, CommandError, ConvergenceError, CursorConvergence,
    CursorPage, CursorRecord, OutputError, OutputMode, RetryGate, TerminalSettlement,
};

fn limits_with(
    stdout_bytes: usize,
    stdout_records: usize,
    retained_cursors: usize,
) -> Result<CliLimits, CliLimitsError> {
    let production = CliLimits::production().values();
    CliLimits::try_new(CliLimitValues {
        stdout_bytes,
        stdout_records,
        retained_cursors,
        ..production
    })
}

#[test]
fn command_grammar_refuses_mixed_connection_and_session_modes(
) -> Result<(), Box<dyn std::error::Error>> {
    let mixed = CliCommand::parse([
        "attach",
        "--service",
        "state",
        "--remote",
        "https://example.test",
        "--credential",
        "basic.credential",
        "--session",
        "ses_a",
    ]);
    assert_eq!(mixed, Err(CommandError::ConnectionMode));

    let sessions = CliCommand::parse([
        "attach",
        "--service",
        "state",
        "--session",
        "ses_a",
        "--new",
    ]);
    assert_eq!(sessions, Err(CommandError::SessionSelector));

    let prompt = CliCommand::parse([
        "run",
        "--service",
        "state",
        "--new",
        "--prompt",
        "hello",
        "--stdin",
    ]);
    assert_eq!(prompt, Err(CommandError::PromptSource));
    let duplicate = CliCommand::parse(["status", "--service", "first", "--service", "second"]);
    assert_eq!(duplicate, Err(CommandError::Duplicate("--service")));
    Ok(())
}

#[test]
fn remote_endpoint_refuses_plaintext_credentials_and_non_root_paths() {
    assert!(ClientEndpoint::remote("http://127.0.0.1:9090").is_err());
    assert!(ClientEndpoint::remote("https://user:secret@example.test").is_err());
    assert!(ClientEndpoint::remote("https://example.test/api").is_err());
    assert!(ClientEndpoint::remote("https://example.test/?redirect=yes").is_err());
    assert!(ClientEndpoint::remote("https://example.test").is_ok());
}

/// Poisoned repository profile and isolated user paths: help must not use them.
struct HelpSandbox(PathBuf);

impl HelpSandbox {
    fn new() -> Result<Self, io::Error> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "grapt-help-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        let sandbox = Self(root);
        std::fs::write(sandbox.0.join(".grapt"), b"not a profile or installation")?;
        Ok(sandbox)
    }

    async fn command(&self, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
            .args(args)
            .current_dir(&self.0)
            .env("HOME", self.0.join("home"))
            .env("USERPROFILE", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("APPDATA", self.0.join("config"))
            .env("LOCALAPPDATA", &self.0)
            .env("PATH", self.0.join("no-programs"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        // Keep the writer open and empty until exit: reading stdin would block.
        let stdin = child.stdin.take().ok_or("missing child stdin pipe")?;
        let output =
            tokio::time::timeout(Duration::from_secs(10), child.wait_with_output()).await??;
        drop(stdin);
        self.unchanged()?;
        Ok(output)
    }

    fn unchanged(&self) -> Result<(), io::Error> {
        let entries = std::fs::read_dir(&self.0)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(entries, [std::ffi::OsString::from(".grapt")]);
        assert_eq!(
            std::fs::read(self.0.join(".grapt"))?,
            b"not a profile or installation"
        );
        Ok(())
    }

    async fn help(&self, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
        let output = self.command(args).await?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
        Ok(String::from_utf8(output.stdout)?)
    }
}

impl Drop for HelpSandbox {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn ephemeral_refusals_precede_installation_profile_and_stdin_effects(
) -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = HelpSandbox::new()?;
    for args in [
        &["code", "--ephemeral"][..],
        &[
            "run",
            "--ephemeral",
            "--embedded",
            "--service",
            "absent",
            "--product",
            "absent-product",
            "--resume",
            "--stdin",
        ][..],
        &[
            "run",
            "--ephemeral",
            "--embedded",
            "--service",
            "absent",
            "--product",
            "absent-product",
            "--prompt",
            "hello",
        ][..],
    ] {
        let output = sandbox.command(args).await?;
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8(output.stderr)?.contains("--ephemeral"));
        assert!(output.stdout.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn root_code_and_installer_help_are_effect_free() -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = HelpSandbox::new()?;
    let root = sandbox.help(&["--help"]).await?;
    assert!(root.starts_with("usage: grapt <command>"));
    for alias in ["-h", "help"] {
        assert_eq!(sandbox.help(&[alias]).await?, root);
        assert_eq!(CliCommand::parse([alias])?, CliCommand::Help);
    }
    for form in [
        "update  --from <package-directory> [--prefix <installation>]",
        "update  --prune [--prefix <installation>]",
        "revert  [--prefix <installation>] [--profile <profile>]",
        "uninstall [--prefix <installation>]",
        "recover-sign-in --profile <profile> [--restore-legacy] (Windows)",
        "grapt update --help",
        "grapt code --help",
    ] {
        assert!(root.contains(form), "missing {form:?}: {root}");
    }
    assert_eq!(CliCommand::parse(["--help"])?, CliCommand::Help);
    for flag in ["--help", "-h"] {
        assert_eq!(
            sandbox.help(&["code", flag]).await?,
            format!("{}\n", grapt_cli::command::terminal::HELP)
        );
        for verb in ["update", "revert", "uninstall"] {
            assert_eq!(
                sandbox.help(&[verb, flag]).await?,
                format!("{}\n", grapt_cli::installation::HELP)
            );
        }
        let recovery = sandbox.help(&["recover-sign-in", flag]).await?;
        assert!(recovery.starts_with("usage: grapt recover-sign-in --profile <profile>"));
        assert!(recovery.contains("[--restore-legacy]"));
        assert!(recovery.contains("Windows only."));
    }
    Ok(())
}

#[tokio::test]
async fn status_help_explains_distinct_effect_free_queries(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_cli::command::help::Topic;

    let sandbox = HelpSandbox::new()?;
    for flag in ["--help", "-h"] {
        for (args, topic) in [
            (vec!["status", flag], Topic::Status),
            (vec!["service", "status", flag], Topic::ServiceStatus),
        ] {
            assert_eq!(
                CliCommand::parse(args.clone())?,
                CliCommand::FocusedHelp(topic)
            );
            assert_eq!(sandbox.help(&args).await?, format!("{}\n", topic.text()));
        }
    }
    let status = sandbox.help(&["status", "--help"]).await?;
    assert!(status.contains("[--json|--jsonl]"));
    assert!(status.contains("`ready <endpoint>`"));
    assert!(status.contains("failure (exit 10)"));
    let compatibility = sandbox.help(&["service", "status", "--help"]).await?;
    assert!(compatibility.contains("stale service (both exit 0)"));
    assert!(compatibility.contains("JSON and JSONL are not supported"));
    for args in [
        vec!["status"],
        vec!["service", "status"],
        vec!["service", "start", "--help"],
        vec!["status", "--service", "root", "--json", "--jsonl"],
        vec!["service", "status", "--service", "root", "--json"],
    ] {
        assert!(CliCommand::parse(args).is_err());
    }
    // A flag-shaped service root is still a value, not a help request.
    assert!(matches!(
        CliCommand::parse(["status", "--service", "--help"])?,
        CliCommand::ServiceStatus { service_root, .. } if service_root.as_path() == std::path::Path::new("--help")
    ));
    Ok(())
}

#[tokio::test]
async fn connection_command_help_bypasses_open_stdin_and_hostile_home(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_cli::command::help::Topic;

    let sandbox = HelpSandbox::new()?;
    for (verb, topic) in [
        ("run", Topic::Run),
        ("attach", Topic::Attach),
        ("acp", Topic::Acp),
    ] {
        for flag in ["--help", "-h"] {
            let args = [verb, flag];
            assert_eq!(CliCommand::parse(args)?, CliCommand::FocusedHelp(topic));
            let help = sandbox.help(&args).await?;
            assert_eq!(help, format!("{}\n", topic.text()));
            for connection in [
                "--service <root>",
                "--remote <https-url> --credential <file>",
                "--embedded --service <root> --product <binary>",
            ] {
                assert!(help.contains(connection), "{verb}: {connection}");
            }
        }
        assert!(CliCommand::parse([verb]).is_err());
    }
    let run = sandbox.help(&["run", "--help"]).await?;
    assert!(run.contains("--stdin reads bounded UTF-8 through EOF"));
    assert!(run.contains("--format json is refused"));
    for hint in [
        "Approve classified requests once, except file removals",
        "(including removals through patch or another tool)",
        "Approve classified requests once, including",
        "Both automatic modes reject owner-decision and unclassified requests",
        "those require an explicit decision",
    ] {
        assert!(
            run.contains(hint),
            "missing auto-approval limitation: {hint}"
        );
    }
    assert!(!run.contains("Approve every request"));
    let attach = sandbox.help(&["attach", "--help"]).await?;
    assert!(attach.contains("choose exactly one"));
    assert!(attach.contains("No prompt, --stdin"));
    let acp = sandbox.help(&["acp", "--help"]).await?;
    assert!(acp.contains("protocol on stdin/stdout"));
    // Preserve help-shaped values, literal messages and non-help refusals.
    for args in [
        vec!["run", "--service", "root", "--prompt", "--help"],
        vec!["run", "--service", "root", "--", "--help"],
    ] {
        assert!(matches!(CliCommand::parse(args)?, CliCommand::Run { .. }));
    }
    for args in [
        vec!["attach", "--service", "root"],
        vec!["attach", "--service", "root", "--new", "--stdin"],
        vec!["acp", "--service", "root", "--session", "ses_example"],
        vec!["run", "--service", "root", "--stdin", "--prompt", "hello"],
    ] {
        assert!(CliCommand::parse(args).is_err());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", windows))]
#[tokio::test]
async fn installer_help_matches_existing_parsers() -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = HelpSandbox::new()?;
    let help = sandbox.help(&["update", "--help"]).await?;
    let blocker = sandbox.0.join(".grapt");
    let blocker = blocker.to_str().ok_or("sandbox path is not UTF-8")?;
    let path_refusal = if cfg!(windows) {
        "installation directory must be real, not a reparse point"
    } else {
        "installation directory must be a real directory, not a link"
    };
    let forms = [
        vec!["update", "--from", "package", "--prefix", blocker],
        vec!["update", "--prune", "--prefix", blocker],
        vec!["revert", "--prefix", blocker, "--profile", "profile"],
        vec!["uninstall", "--prefix", blocker],
    ];
    for form in &forms {
        assert!(help.contains(&format!("grapt {}", form[0])), "{help}");
        // A regular file cannot be an installation: accepted syntax must fail
        // at path validation, before a lock, service, package or launcher effect.
        let output = sandbox.command(form).await?;
        assert_eq!(output.status.code(), Some(2), "{form:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{form:?}: {output:?}");
        let error = String::from_utf8(output.stderr)?;
        assert_eq!(error, format!("grapt: {path_refusal}\n"), "{form:?}");
    }
    assert!(help.contains("grapt update --prune [--prefix <installation>]\n"));
    assert_eq!(help.contains("[--no-path]"), cfg!(windows));
    assert_eq!(help.contains("[--refresh-launcher]"), cfg!(windows));
    for (index, form) in forms.iter().enumerate() {
        for flag in ["--no-path", "--refresh-launcher"] {
            let mut args = form.clone();
            args.push(flag);
            let output = sandbox.command(&args).await?;
            assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
            assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
            let error = String::from_utf8(output.stderr)?;
            let supported = cfg!(windows)
                && if flag == "--no-path" {
                    index != 1
                } else {
                    index == 0
                };
            let reason = if supported {
                path_refusal
            } else {
                grapt_cli::installation::HELP
            };
            assert_eq!(error, format!("grapt: {reason}\n"), "{args:?}");
        }
    }
    #[cfg(windows)]
    {
        let mut args = forms[0].clone();
        args.extend(["--no-path", "--refresh-launcher"]);
        let output = sandbox.command(&args).await?;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr)?,
            format!("grapt: {path_refusal}\n")
        );
    }
    Ok(())
}

#[test]
fn output_becomes_visible_only_after_a_complete_bounded_record(
) -> Result<(), Box<dyn std::error::Error>> {
    let limits = limits_with(8, 1, 2)?;
    let mut output = BoundedOutput::new(Vec::new(), Vec::new(), OutputMode::Text, limits);
    assert!(!output.receipt().visible);
    output.text("ok")?;
    assert!(output.receipt().visible);
    assert_eq!(output.receipt().stdout_bytes, 3);
    assert!(matches!(
        output.text("again"),
        Err(OutputError::StdoutRecords { maximum: 1 })
    ));
    assert_eq!(output.receipt().stdout_records, 1);
    Ok(())
}

#[test]
fn broken_pipe_is_terminal_and_never_claims_visible_output(
) -> Result<(), Box<dyn std::error::Error>> {
    let limits = limits_with(8, 1, 2)?;
    let mut output = BoundedOutput::new(BrokenWriter, Vec::new(), OutputMode::Text, limits);
    assert!(matches!(output.text("ok"), Err(OutputError::BrokenPipe)));
    assert!(!output.receipt().visible);
    assert_eq!(output.receipt().stdout_records, 0);
    Ok(())
}

#[test]
fn a_late_advisory_is_skipped_while_durable_pages_stay_strictly_ordered(
) -> Result<(), Box<dyn std::error::Error>> {
    let limits = limits_with(64, 8, 4)?;
    let mut convergence = CursorConvergence::new(limits);
    let later = CursorRecord::try_new("c2", 2, b"two".to_vec())?;
    let earlier = CursorRecord::try_new("c1", 1, b"one".to_vec())?;
    convergence.admit_advisory(CursorPage::try_new(vec![later], None, limits)?)?;
    let late =
        convergence.admit_advisory(CursorPage::try_new(vec![earlier.clone()], None, limits)?);
    assert!(
        matches!(&late, Ok(admitted) if admitted.is_empty()),
        "a late live advisory failed the run instead of deferring to durable truth: {late:?}"
    );
    assert_eq!(convergence.settled(), Some(2));
    assert_eq!(
        convergence.admit_page(CursorPage::try_new(vec![earlier], None, limits)?),
        Err(ConvergenceError::OutOfOrder {
            settled: 2,
            found: 1
        })
    );
    Ok(())
}

#[test]
fn convergence_reconciles_exact_duplicates_and_refuses_gap_conflict_and_reuse(
) -> Result<(), Box<dyn std::error::Error>> {
    let limits = limits_with(64, 8, 4)?;
    let first = CursorRecord::try_new("c1", 1, b"one".to_vec())?;
    let mut convergence = CursorConvergence::new(limits);
    let admitted = convergence.admit_page(CursorPage::try_new(
        vec![first.clone(), first],
        Some("c1".to_owned()),
        limits,
    )?)?;
    assert_eq!(admitted.len(), 1);
    assert_eq!(convergence.settled(), Some(1));

    let gap = CursorPage::try_new(
        vec![CursorRecord::try_new("c3", 3, b"three".to_vec())?],
        None,
        limits,
    )?;
    assert_eq!(
        convergence.admit_page(gap),
        Err(ConvergenceError::Gap {
            expected: 2,
            found: 3,
        })
    );

    let conflict = CursorPage::try_new(
        vec![CursorRecord::try_new("c1", 1, b"changed".to_vec())?],
        None,
        limits,
    )?;
    assert_eq!(
        convergence.admit_page(conflict),
        Err(ConvergenceError::ConflictingDuplicate { sequence: 1 })
    );
    Ok(())
}

#[test]
fn advisory_sse_is_typed_and_requires_durable_relist() -> Result<(), Box<dyn std::error::Error>> {
    let limits = CliLimits::production();
    let frame = br#"event: message
data: {"type":"session.text.ended","id":"evt_1","created":1.0,"durable":{"aggregateID":"ses_a","seq":7,"version":1},"data":{"sessionID":"ses_a","assistantMessageID":"msg_a","ordinal":0,"text":"done"}}"#;
    let event = AdvisoryEvent::decode_sse(frame, limits)?;
    assert_eq!(event.kind(), "session.text.ended");
    assert_eq!(
        event.session_id().map(ToString::to_string).as_deref(),
        Some("ses_a")
    );
    assert!(event.requires_durable_relist());
    assert_eq!(
        event
            .cursor_record()?
            .ok_or("missing durable cursor")?
            .sequence(),
        7
    );

    let unknown = br#"event: message
data: {"type":"provider.secret.delta","id":"evt_2","created":1.0,"data":{}}"#;
    assert_eq!(
        AdvisoryEvent::decode_sse(unknown, limits),
        Err(AdvisoryError::EventKind)
    );
    let explicit_null = br#"event: message
data: {"type":"session.text.delta","id":"evt_3","created":1.0,"durable":null,"data":{"sessionID":"ses_a","assistantMessageID":"msg_a","ordinal":0,"delta":"x"}}"#;
    assert_eq!(
        AdvisoryEvent::decode_sse(explicit_null, limits),
        Err(AdvisoryError::DurableReference)
    );
    Ok(())
}

#[test]
fn advisory_sse_admits_execution_started_at_exact_durable_version(
) -> Result<(), Box<dyn std::error::Error>> {
    let frame = br#"event: message
data: {"type":"session.execution.started","id":"evt_1","created":1.0,"durable":{"aggregateID":"ses_a","seq":7,"version":1},"data":{"sessionID":"ses_a","executionID":"exe_a"}}"#;
    assert!(
        AdvisoryEvent::decode_sse(frame, CliLimits::production()).is_ok(),
        "execution-start event was rejected before the ACP client could observe the prompt"
    );
    Ok(())
}

#[test]
fn advisory_sse_admits_compaction_ended_at_exact_durable_version(
) -> Result<(), Box<dyn std::error::Error>> {
    // The live notice the product publishes when any compaction commits.
    let frame = br#"event: message
data: {"type":"session.compaction.ended","id":"evt_1","created":1.0,"durable":{"aggregateID":"ses_a","seq":9,"version":1},"data":{"sessionID":"ses_a","reason":"auto","text":"summary","recent":""}}"#;
    let event = AdvisoryEvent::decode_sse(frame, CliLimits::production())
        .map_err(|error| format!("a compaction notice ended the ACP turn: {error:?}"))?;
    assert_eq!(event.kind(), "session.compaction.ended");
    assert!(event.requires_durable_relist());
    let wrong_version = br#"event: message
data: {"type":"session.compaction.ended","id":"evt_2","created":1.0,"durable":{"aggregateID":"ses_a","seq":9,"version":2},"data":{"sessionID":"ses_a","reason":"auto","text":"summary","recent":""}}"#;
    assert!(AdvisoryEvent::decode_sse(wrong_version, CliLimits::production()).is_err());
    Ok(())
}

#[test]
fn advisory_sse_admits_every_kind_the_product_publishes() -> Result<(), Box<dyn std::error::Error>>
{
    // Selection events (durable v1) and the live web-search notice: each is
    // published while a turn may be streaming, so refusing one ended it.
    for kind in [
        "session.model.selected",
        "session.agent.selected",
        "session.subagent-model.selected",
        "session.subagent-model.cleared",
    ] {
        let frame = format!(
            "event: message\ndata: {{\"type\":\"{kind}\",\"id\":\"evt_1\",\"created\":1.0,\"durable\":{{\"aggregateID\":\"ses_a\",\"seq\":3,\"version\":1}},\"data\":{{\"sessionID\":\"ses_a\"}}}}"
        );
        AdvisoryEvent::decode_sse(frame.as_bytes(), CliLimits::production())
            .map_err(|error| format!("{kind} was refused: {error:?}"))?;
    }
    // The web-search notice is global: the product publishes it with empty
    // data (WebSearchUpdatedData::default), never with a Session.
    let search = br#"event: message
data: {"type":"websearch.updated","id":"evt_2","created":1.0,"data":{}}"#;
    let notice = AdvisoryEvent::decode_sse(search, CliLimits::production())
        .map_err(|error| format!("websearch.updated was refused: {error:?}"))?;
    assert_eq!(notice.session_id(), None);
    let scoped = br#"event: message
data: {"type":"websearch.updated","id":"evt_3","created":1.0,"data":{"sessionID":"ses_a"}}"#;
    assert_eq!(
        AdvisoryEvent::decode_sse(scoped, CliLimits::production()),
        Err(AdvisoryError::SessionIdentity)
    );
    Ok(())
}

#[test]
fn advisory_sse_refuses_oversized_frames_before_decode() -> Result<(), Box<dyn std::error::Error>> {
    let production = CliLimits::production().values();
    let limits = CliLimits::try_new(CliLimitValues {
        sse_frame_bytes: 32,
        ..production
    })?;
    let oversized = vec![b'x'; 33];
    assert_eq!(
        AdvisoryEvent::decode_sse(&oversized, limits),
        Err(AdvisoryError::FrameBound { maximum: 32 })
    );
    Ok(())
}

#[test]
fn advisory_sse_refuses_a_known_kind_at_the_wrong_durable_version(
) -> Result<(), Box<dyn std::error::Error>> {
    let limits = CliLimits::production();
    let wrong = br#"event: message
data: {"type":"permission.asked","id":"evt_1","created":1.0,"durable":{"aggregateID":"ses_a","seq":7,"version":1},"data":{"sessionID":"ses_a"}}"#;
    assert_eq!(
        AdvisoryEvent::decode_sse(wrong, limits),
        Err(AdvisoryError::DurableReference),
        "wrong durable event version was accepted"
    );

    let exact = br#"event: message
data: {"type":"permission.asked","id":"evt_1","created":1.0,"durable":{"aggregateID":"ses_a","seq":7,"version":2},"data":{"sessionID":"ses_a"}}"#;
    assert!(AdvisoryEvent::decode_sse(exact, limits).is_ok());
    Ok(())
}

#[test]
fn visible_output_and_non_idempotent_operations_are_never_retried(
) -> Result<(), Box<dyn std::error::Error>> {
    let gate = RetryGate::new(3)?;
    assert!(gate.allows(false, 1, true));
    assert!(!gate.allows(true, 1, true));
    assert!(!gate.allows(false, 1, false));
    assert!(!gate.allows(false, 3, true));
    Ok(())
}

#[test]
fn compound_visible_output_plus_retry_is_impossible() -> Result<(), Box<dyn std::error::Error>> {
    let gate = RetryGate::new(3)?;
    let output_was_visible = true;
    let operation_was_idempotent = true;
    assert!(
        !gate.allows(output_was_visible, 1, operation_was_idempotent),
        "a visible record must permanently close the retry door"
    );
    Ok(())
}

#[test]
fn cancellation_and_terminal_truth_cannot_both_settle() -> Result<(), Box<dyn std::error::Error>> {
    let mut cancelled = TerminalSettlement::Pending;
    cancelled.settle_cancel(CancellationCause::Sigint)?;
    assert!(cancelled.settle_terminal().is_err());

    let mut terminal = TerminalSettlement::Pending;
    terminal.settle_terminal()?;
    assert!(terminal.settle_cancel(CancellationCause::Sigterm).is_err());
    Ok(())
}

#[test]
fn dependency_direction_is_typed_api_client_only() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml"))?)?;
    let dependencies = manifest["dependencies"]
        .as_table()
        .ok_or("dependencies are not a table")?;
    for forbidden in [
        "grapt-core",
        "grapt-runtime",
        "grapt-store",
        "grapt-product",
        "grapt-provider",
    ] {
        assert!(
            !dependencies.contains_key(forbidden),
            "CLI dependency direction admitted {forbidden}"
        );
    }
    for entry in std::fs::read_dir(root.join("src"))? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path)?;
        for forbidden in [
            "grapt_core",
            "grapt_runtime",
            "grapt_store",
            "grapt_product",
            "unbounded_channel",
            "std::sync::mpsc",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} admitted forbidden dependency/path {forbidden}",
                path.display()
            );
        }
    }
    Ok(())
}

#[derive(Debug)]
struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

//! Real direct-executable preflight: no profile, terminal, or service is needed.
use std::{
    fs,
    io::Read,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "grapt-direct-tui-preflight-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        let sandbox = Self(root);
        fs::write(sandbox.0.join(".grapt"), b"not a profile")?;
        Ok(sandbox)
    }

    fn command(&self, args: &[&str]) -> Result<Output> {
        // Every user-config directory points at a regular file. Profiling would
        // create a new file if preflight accidentally entered the UI owner.
        let blocker = self.0.join(".grapt");
        let mut process = Command::new(env!("CARGO_BIN_EXE_grapt-tui"));
        process
            .env_clear()
            .args(args)
            .current_dir(&self.0)
            .env("HOME", &blocker)
            .env("USERPROFILE", &blocker)
            .env("APPDATA", &blocker)
            .env("XDG_CONFIG_HOME", &blocker)
            .env("GRAPT_TUI_R_PROFILE", self.0.join("frames.jsonl"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(system) = std::env::var_os("SystemRoot") {
            process.env("SystemRoot", system);
        }
        let mut child = Owned(process.spawn()?);
        for _ in 0..200 {
            if let Some(status) = child.0.try_wait()? {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                child
                    .0
                    .stdout
                    .take()
                    .ok_or("stdout absent")?
                    .take(8193)
                    .read_to_end(&mut stdout)?;
                child
                    .0
                    .stderr
                    .take()
                    .ok_or("stderr absent")?
                    .take(8193)
                    .read_to_end(&mut stderr)?;
                assert!(stdout.len() <= 8192 && stderr.len() <= 8192);
                let entries = fs::read_dir(&self.0)?
                    .map(|entry| entry.map(|entry| entry.file_name()))
                    .collect::<std::io::Result<Vec<_>>>()?;
                assert_eq!(entries, [std::ffi::OsString::from(".grapt")]);
                assert_eq!(fs::read(blocker)?, b"not a profile");
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err("direct TUI preflight did not finish within five seconds".into())
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _killed = self.0.kill();
        let _reaped = self.0.wait();
    }
}

#[test]
fn direct_help_advertises_display_flags_without_profile_or_terminal_effects() -> Result {
    let sandbox = Sandbox::new()?;
    let mut previous = None;
    for flag in ["--help", "-h"] {
        let output = sandbox.command(&[flag])?;
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let text = String::from_utf8(output.stdout)?;
        assert!(text.starts_with("Grapt native terminal\n"));
        assert!(!text.contains('\u{1b}'));
        for hint in [
            "--light selects a light theme",
            "--dark selects a dark theme (default)",
            "the last one wins",
            "Notifications are enabled by default",
            "--no-notifications disables them",
        ] {
            assert!(text.contains(hint), "missing {hint}: {text}");
        }
        if let Some(previous) = previous {
            assert_eq!(text, previous);
        }
        previous = Some(text);
    }
    Ok(())
}

#[test]
fn malformed_direct_launch_options_fail_before_profile_or_terminal_effects() -> Result {
    let sandbox = Sandbox::new()?;
    for (args, reason) in [
        (vec!["--service"], "missing value for --service"),
        (vec!["--directory"], "missing value for --directory"),
        (vec!["--session"], "missing value for --session"),
        (vec!["--prompt"], "missing value for --prompt"),
        (
            vec!["--service", "FIRST_SECRET", "--service", "SECOND_SECRET"],
            "duplicate --service option",
        ),
        (
            vec![
                "--directory",
                "FIRST_SECRET",
                "--directory",
                "SECOND_SECRET",
            ],
            "duplicate --directory option",
        ),
        (
            vec!["--session", "FIRST_SECRET", "--session", "SECOND_SECRET"],
            "duplicate --session option",
        ),
        (
            vec!["--prompt", "FIRST_SECRET", "--prompt", "SECOND_SECRET"],
            "duplicate --prompt option",
        ),
        (
            vec!["--light", "--bogus=FIRST_SECRET"],
            "unknown launch option --bogus",
        ),
        (
            vec!["--dark", "--no-notifications", "FIRST_SECRET"],
            "unknown launch option",
        ),
        (
            vec!["--service", "FIRST_SECRET\u{1b}"],
            "invalid value for --service",
        ),
    ] {
        let output = sandbox.command(&args)?;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stderr)?,
            format!("grapt-tui: {reason}\nRun `grapt-tui --help` for usage.\n")
        );
    }
    Ok(())
}

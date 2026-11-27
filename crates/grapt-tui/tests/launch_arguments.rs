//! Unix OS strings must be refused before direct-TUI resource acquisition.
#![cfg(unix)]

use std::{
    ffi::OsString,
    fs,
    io::Read,
    os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "grapt-native-argv-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        let sandbox = Self(path);
        fs::set_permissions(&sandbox.0, fs::Permissions::from_mode(0o700))?;
        fs::write(sandbox.0.join(".grapt"), b"not a profile")?;
        Ok(sandbox)
    }

    fn command(&self, args: &[OsString]) -> Result<Output> {
        let blocker = self.0.join(".grapt");
        let mut child = Owned(
            Command::new(env!("CARGO_BIN_EXE_grapt-tui"))
                .args(args)
                .env_clear()
                .env("HOME", &blocker)
                .env("USERPROFILE", &blocker)
                .env("APPDATA", &blocker)
                .env("XDG_CONFIG_HOME", &blocker)
                .env("XDG_DATA_HOME", &blocker)
                .env("RUST_BACKTRACE", "1")
                .env("GRAPT_TUI_R_PROFILE", self.0.join("frames.jsonl"))
                .current_dir(&self.0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
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
                assert_eq!(entries, [OsString::from(".grapt")]);
                assert_eq!(fs::read(&blocker)?, b"not a profile");
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err("direct TUI preflight exceeded five seconds".into())
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
fn non_unicode_argv_is_a_sanitized_configuration_refusal_before_resources() -> Result {
    let sandbox = Sandbox::new()?;
    let invalid = OsString::from_vec(b"PRIVATE_ARG\xff\n\x1b[31m".to_vec());
    for prefix in [
        vec![],
        vec!["--help"],
        vec!["--version"],
        vec!["--service"],
        vec!["--service", "profile", "--directory"],
        vec!["--service", "profile", "--session"],
        vec![
            "--service",
            "profile",
            "--session",
            "ses_000000000001",
            "--prompt",
        ],
    ] {
        let args: Vec<_> = prefix
            .into_iter()
            .map(OsString::from)
            .chain([invalid.clone()])
            .collect();
        let output = sandbox.command(&args)?;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"grapt-tui: launch arguments must be valid Unicode\nRun `grapt-tui --help` for usage.\n");
    }
    Ok(())
}

#[test]
fn unicode_configuration_and_help_version_preflight_keep_their_behavior() -> Result {
    let sandbox = Sandbox::new()?;
    for flag in ["--help", "-h", "--version", "-V"] {
        let output = sandbox.command(&[flag.into()])?;
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout)?;
        if flag == "--version" || flag == "-V" {
            assert_eq!(text, concat!("grapt-tui ", env!("CARGO_PKG_VERSION"), "\n"));
        } else {
            assert!(text.starts_with("Grapt native terminal\n"));
        }
    }
    let output = sandbox.command(&["--directory".into(), "é-目录".into()])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"grapt-tui: --service is required\n");
    Ok(())
}

#[test]
fn unix_launch_without_a_service_reports_a_refusal_instead_of_panicking() -> Result {
    let sandbox = Sandbox::new()?;
    let mut child = Owned(
        Command::new(env!("CARGO_BIN_EXE_grapt-tui"))
            .args(["--service", ".", "--directory", "."])
            .env_clear()
            .current_dir(&sandbox.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    for _ in 0..200 {
        if let Some(status) = child.0.try_wait()? {
            let mut stderr = String::new();
            child
                .0
                .stderr
                .take()
                .ok_or("stderr absent")?
                .take(8193)
                .read_to_string(&mut stderr)?;
            assert!(stderr.len() <= 8192);
            assert_eq!(status.code(), Some(1), "{stderr}");
            assert!(stderr.starts_with("grapt-tui:"), "{stderr}");
            assert!(!stderr.contains("panicked"), "{stderr}");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("direct TUI service refusal exceeded five seconds".into())
}

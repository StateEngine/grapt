//! A diagnostic sink cannot retain a foreground service after control EOF.
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::thread;
use std::time::Duration;

use grapt_store::{apply_migrations, EventStore, ProductionMigrations};
use nix::fcntl::{fcntl, FcntlArg};

#[path = "../support/poll_deadline.rs"]
mod poll_deadline;
use poll_deadline::PollDeadline;

#[cfg(feature = "loopback-provider-testkit")]
use super::scripted_provider::Server as ScriptedProvider;
#[cfg(feature = "loopback-provider-testkit")]
#[path = "shutdown_diagnostics/compaction.rs"]
mod compaction;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const PIPE_BYTES: usize = 8192;

#[derive(Debug)]
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_ready(child: &mut Child, path: &Path) -> TestResult {
    let deadline = PollDeadline::new();
    while deadline.pending() {
        if let Some(status) = child.try_wait()? {
            return Err(format!("server exited before readiness: {status}").into());
        }
        match File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(8193).read_to_end(&mut bytes)?;
                assert!(bytes.len() <= 8192, "readiness exceeded its test bound");
                match serde_json::from_slice::<serde_json::Value>(&bytes) {
                    Ok(ready) => {
                        assert_eq!(ready["pid"].as_u64(), Some(u64::from(child.id())));
                        return Ok(());
                    }
                    Err(error) if error.is_eof() => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(format!("server never published readiness ({deadline})").into())
}

fn launch_with_provider(
    root: &Path,
    ephemeral: bool,
    provider: Option<std::net::SocketAddr>,
) -> TestResult<(Running, File, File)> {
    super::write_inputs(root);
    if let Some(address) = provider {
        super::provider_profile::configure(root, address)?;
    }
    if !ephemeral {
        drop(EventStore::open(&root.join("database.sqlite3"), "0.1.0")?);
        let migrations = ProductionMigrations::load()?;
        apply_migrations(
            &root.join("database.sqlite3"),
            root,
            "0.1.0",
            &migrations.as_slice(),
        )?;
    }
    let (read, write) = nix::unistd::pipe()?;
    assert_eq!(
        fcntl(&read, FcntlArg::F_SETPIPE_SZ(PIPE_BYTES as i32))?,
        8192
    );
    let stderr = File::from(read);
    let stdout = root.join("stdout.log");
    let mut command =
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"));
    command
        .current_dir(root)
        .env_remove("GRAPT_CONFIG")
        .env_remove("SECODE_CONFIG") // grapt-rename: keep
        .env_remove("GRAPT_CONFIG_CONTENT")
        .env_remove("SECODE_CONFIG_CONTENT") // grapt-rename: keep
        .env("GRAPT_TEST_HOME", root.join("home"))
        .env("GRAPT_CONFIG_DIR", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"));
    if let Some(address) = provider {
        command.env("GRAPT_LIVE_HEADLESS_LOOPBACK", format!("http://{address}"));
    }
    if ephemeral {
        command.args(["serve-ephemeral", "--service"]).arg(root);
    } else {
        command
            .args(["serve-terminal", "--service"])
            .arg(root)
            .args(["--port", "0"]);
    }
    let mut running = Running(
        command
            .stdin(Stdio::piped())
            .stdout(File::create(&stdout)?)
            .stderr(Stdio::from(write.try_clone()?))
            .spawn()?,
    );
    // Command retains its copy of the write descriptor until it is dropped.
    drop(command);
    let ready_path = if ephemeral {
        stdout
    } else {
        root.join("serve-ready.json")
    };
    wait_ready(&mut running.0, &ready_path)?;
    Ok((running, stderr, File::from(write)))
}

fn fill_stderr(
    mut writer: File,
    stderr: &File,
) -> TestResult<thread::JoinHandle<std::io::Result<()>>> {
    // Write one byte at a time so even the pipe's last free byte is filled.
    // Attempting one byte beyond its capacity leaves this finite producer
    // blocked too. Neither the server's descriptor flags nor its code change.
    let filler = thread::spawn(move || -> std::io::Result<()> {
        for _ in 0..=PIPE_BYTES {
            writer.write_all(b".")?;
        }
        Ok(())
    });
    let fill_deadline = PollDeadline::new();
    while rustix::io::ioctl_fionread(stderr)? != PIPE_BYTES as u64 {
        assert!(
            fill_deadline.pending(),
            "stderr pipe did not fill ({fill_deadline})"
        );
        thread::sleep(Duration::from_millis(10));
    }
    Ok(filler)
}

fn finish(
    root: &Path,
    mut running: Running,
    stderr: File,
    filler: thread::JoinHandle<std::io::Result<()>>,
) -> TestResult {
    assert!(
        running.0.try_wait()?.is_none(),
        "server stopped before control EOF"
    );
    drop(running.0.stdin.take());
    let deadline = PollDeadline::new();
    let drained = deadline
        .poll(Duration::from_millis(10), || {
            running.0.try_wait().transpose()
        })
        .transpose()?;
    if drained.is_none() {
        running.0.kill()?;
        running.0.wait()?;
    }
    // Read only after the exit decision. Releasing backpressure earlier would
    // hide the regression; after exit this also lets the finite filler join.
    let mut diagnostics = Vec::new();
    stderr
        .take((2 * PIPE_BYTES + 2) as u64)
        .read_to_end(&mut diagnostics)?;
    assert!(
        diagnostics.len() <= 2 * PIPE_BYTES + 1,
        "diagnostics exceeded cleanup bound"
    );
    filler.join().map_err(|_| "stderr filler panicked")??;
    assert!(
        drained.is_some(),
        "service did not drain after control EOF ({deadline})"
    );
    assert!(
        drained.is_some_and(|status| status.success()),
        "server exited unsuccessfully"
    );
    assert!(!root.join("serve-ready.json").try_exists()?);
    assert!(!root.join("registration").try_exists()?);
    Ok(())
}

fn exercise(ephemeral: bool) -> TestResult {
    let root = tempfile::tempdir()?;
    let (running, stderr, writer) = launch_with_provider(root.path(), ephemeral, None)?;
    let filler = fill_stderr(writer, &stderr)?;
    finish(root.path(), running, stderr, filler)
}

#[test]
fn terminal_service_drains_with_a_full_unread_stderr_pipe() -> TestResult {
    exercise(false)
}

#[test]
fn ephemeral_service_drains_with_a_full_unread_stderr_pipe() -> TestResult {
    exercise(true)
}

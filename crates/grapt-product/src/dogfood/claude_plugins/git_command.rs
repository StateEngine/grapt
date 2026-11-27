//! Private Git subprocesses use the same bounded lifetime pattern as lifecycle hooks.
//! The caller owns a fresh scratch directory; repository files are never checked out.
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

const REAP: Duration = Duration::from_secs(2);

pub(super) struct Error {
    pub message: String,
    pub unreaped: bool,
}
impl From<String> for Error {
    fn from(message: String) -> Self {
        Self {
            message,
            unreaped: false,
        }
    }
}
impl From<&str> for Error {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

pub(super) async fn run(
    program: &OsStr,
    scratch: &Path,
    arguments: &[&str],
    output_bytes: usize,
) -> Result<Vec<u8>, Error> {
    let mut command = tokio::process::Command::new(program);
    command
        .arg(format!("--git-dir={}", scratch.display()))
        .args([
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "credential.helper=",
            "-c",
            "core.askPass=",
            "-c",
            "core.hooksPath=",
            "-c",
            "fetch.recurseSubmodules=false",
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
            "-c",
            "http.followRedirects=false",
            "-c",
            "http.lowSpeedLimit=1",
            "-c",
            "http.lowSpeedTime=15",
        ])
        .args(arguments)
        .current_dir(scratch)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in ["PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", null)
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("HOME", scratch)
        .env("USERPROFILE", scratch)
        .env("XDG_CONFIG_HOME", scratch)
        .env("GIT_TEMPLATE_DIR", OsString::new());
    grapt_service::launcher_environment::scrub(command.as_std_mut());
    let group = processkit::ProcessGroup::new()
        .map_err(|_| "plugin Git containment could not be created")?;
    let mut child = group.spawn(command).map_err(|_| {
        "plugin Git could not start; install Git and check its executable permissions"
    })?;
    let stdout = child.stdout.take().ok_or("plugin Git stdout is absent")?;
    let stderr = child.stderr.take().ok_or("plugin Git stderr is absent")?;
    let collect = async {
        let wait = async {
            let status = child.wait().await.map_err(|_| "plugin Git wait failed")?;
            group
                .kill_all()
                .map_err(|_| "plugin Git descendant cleanup failed")?;
            Ok::<_, String>(status)
        };
        let work = async {
            let (stdout, _, status) = tokio::try_join!(
                capture(stdout, output_bytes),
                capture(stderr, 16 * 1024),
                wait
            )?;
            if !status.success() {
                return Err(
                    "plugin Git operation failed; check the remote source and revision".into(),
                );
            }
            Ok(stdout)
        };
        tokio::select! {
            result = work => result,
            result = monitor(scratch) => result.and(Err("plugin Git scratch monitor ended".into())),
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(120), collect).await;
    let killed = group.kill_all();
    let reaped = tokio::time::timeout(REAP, child.wait()).await;
    if killed.is_err() || !matches!(reaped, Ok(Ok(_))) {
        return Err(Error {
            message: "plugin Git cleanup could not confirm process exit".into(),
            unreaped: true,
        });
    }
    result
        .map_err(|_| "plugin Git operation exceeded 120 seconds")?
        .map_err(Error::from)
}

async fn capture(mut stream: impl AsyncRead + Unpin, maximum: usize) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let available = buffer.len().min(maximum - output.len() + 1);
        let count = stream
            .read(&mut buffer[..available])
            .await
            .map_err(|_| "plugin Git output read failed")?;
        if count == 0 {
            return Ok(output);
        }
        if count > maximum - output.len() {
            return Err("plugin Git output exceeded its bound".into());
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

async fn monitor(root: &Path) -> Result<(), String> {
    loop {
        scratch_bound(root).await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
async fn scratch_bound(root: &Path) -> Result<(), String> {
    let mut pending = vec![root.to_owned()];
    let mut entries = 0usize;
    let mut bytes = 0u64;
    while let Some(directory) = pending.pop() {
        let mut reader = tokio::fs::read_dir(directory)
            .await
            .map_err(super::snapshot::io)?;
        while let Some(entry) = reader.next_entry().await.map_err(super::snapshot::io)? {
            entries += 1;
            if entries > 8192 {
                return Err("plugin Git scratch exceeds 8192 entries".into());
            }
            let metadata = match tokio::fs::symlink_metadata(entry.path()).await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(super::snapshot::io(error)),
            };
            super::snapshot::unlinked(&metadata)?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
                if bytes > 192 * 1024 * 1024 {
                    return Err("plugin Git scratch exceeds 192 MiB".into());
                }
            } else {
                return Err("plugin Git scratch contains a special file".into());
            }
        }
    }
    Ok(())
}

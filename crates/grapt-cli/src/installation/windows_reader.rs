//! Bounded execution of Windows security readers shipped by the operating system.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::{
    acl::{self, Refusal},
    io,
};

const POLL: Duration = Duration::from_millis(20);
const POLLS: usize = 3000;
const OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Preserve verbatim-path handling without ever selecting a lossy path alias.
pub(super) fn command_path(path: &Path) -> Result<String, Refusal> {
    path.to_str().ok_or_else(|| {
        Refusal::Unverifiable(
            "Windows security readers require Unicode paths; retry with Unicode package, installation and temporary-directory paths".into(),
        )
    })?;
    Ok(super::display_path(path))
}

pub(super) struct Scratch(pub(super) PathBuf);

impl Scratch {
    pub(super) fn new() -> Result<Self, Refusal> {
        let path = std::env::temp_dir().join(format!(
            "grapt-security-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).map_err(|error| {
            Refusal::Unverifiable(format!(
                "scratch directory cannot be created: {:?}",
                error.kind()
            ))
        })?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

fn said(stderr: &Path) -> String {
    let bytes = io::read(stderr, OUTPUT_BYTES).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    let text = if text.contains("<Objs") {
        acl::powershell_errors(&text)
    } else {
        text.into_owned()
    };
    let line: String = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    if line.is_empty() {
        String::new()
    } else {
        format!(": {line}")
    }
}

pub(super) fn run(
    program: &Path,
    arguments: &[OsString],
    scratch: &Path,
    name: &str,
    unset: &[&str],
) -> Result<Vec<u8>, Refusal> {
    run_at(program, arguments, scratch, name, unset, None, None)
}

pub(super) fn run_at(
    program: &Path,
    arguments: &[OsString],
    scratch: &Path,
    name: &str,
    unset: &[&str],
    directory: Option<&Path>,
    input_path: Option<&Path>,
) -> Result<Vec<u8>, Refusal> {
    let shown = super::display_path(program);
    let output = scratch.join(name);
    let errors = scratch.join(format!("{name}.err"));
    let created = |path: &Path| {
        std::fs::File::create_new(path).map_err(|error| {
            Refusal::Unverifiable(format!(
                "{name} output cannot be created: {:?}",
                error.kind()
            ))
        })
    };
    let (file, error_file) = (created(&output)?, created(&errors)?);
    let mut command = grapt_service::launcher_environment::command(program);
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    for variable in unset {
        command.env_remove(variable);
    }
    if let Some(path) = input_path {
        // A child-only environment value carries the path as data, never script text.
        command.env("GRAPT_SECURITY_PATH", command_path(path)?);
    }
    let mut child = command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(error_file))
        .spawn()
        .map_err(|error| {
            Refusal::Unverifiable(format!("{shown} cannot start: {:?}", error.kind()))
        })?;
    for _ in 0..POLLS {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return io::read(&output, OUTPUT_BYTES).map_err(Refusal::Unverifiable);
            }
            Ok(Some(status)) => {
                return Err(Refusal::Unverifiable(format!(
                    "{shown} failed ({status}){}",
                    said(&errors)
                )))
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(error) => {
                let _kill = child.kill();
                let _join = child.wait();
                return Err(Refusal::Unverifiable(format!(
                    "{shown} cannot be waited for: {:?}",
                    error.kind()
                )));
            }
        }
    }
    let _kill = child.kill();
    let _join = child.wait();
    Err(Refusal::Unverifiable(format!(
        "{shown} did not finish within 60 seconds"
    )))
}

pub(super) fn powershell(script: &str, input_path: &Path) -> Result<Vec<u8>, String> {
    let system = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
        .ok_or("SystemRoot is not an absolute path")?;
    let shell = system.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let scratch = Scratch::new().map_err(|error| error.message())?;
    let encoded = acl::encoded_command(script);
    let arguments = [
        "-NoProfile",
        "-NonInteractive",
        "-NoLogo",
        "-EncodedCommand",
        &encoded,
    ]
    .map(OsString::from);
    run_at(
        &shell,
        &arguments,
        &scratch.0,
        "signature.out",
        &["PSModulePath"],
        None,
        Some(input_path),
    )
    .map_err(|error| error.message())
}

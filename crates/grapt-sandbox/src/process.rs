//! Argument-preserving process launch for supervised, piped protocols.
use crate::{Policy, ShellRequest, SpawnFailure};
use std::path::PathBuf;

/// Already admitted executable and arguments; no shell source is accepted.
#[derive(Clone)]
pub struct ProcessRequest {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
    pub environment: Vec<String>,
    pub policy: Policy,
}

impl std::fmt::Debug for ProcessRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessRequest")
            .field("executable", &self.executable)
            .field("working_directory", &self.working_directory)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl ProcessRequest {
    pub(crate) fn validate(&self) -> Result<(), SpawnFailure> {
        if !self.executable.is_absolute()
            || self
                .executable
                .to_str()
                .is_none_or(|path| path.len() > 32 * 1024 || path.contains('\0'))
            || self.arguments.len() > 256
            || self.arguments.iter().any(|arg| arg.contains('\0'))
            || self.arguments.iter().map(String::len).sum::<usize>() > 64 * 1024
        {
            return Err(SpawnFailure::for_backend(
                "process",
                "sandbox needs an absolute executable and bounded arguments",
                false,
            ));
        }
        self.shell_request(String::from("process"))
            .validate()
            .map_err(|reason| SpawnFailure::for_backend("process", reason, false))
    }

    fn shell_request(&self, command_line: String) -> ShellRequest {
        ShellRequest {
            command_line,
            working_directory: self.working_directory.clone(),
            environment: self.environment.clone(),
            // Validate the bounded process inputs through the shell policy.
            // Piped native execution delegates lifetime to its caller.
            timeout_ms: 86_400_000,
            policy: self.policy.clone(),
        }
    }
}

#[cfg(windows)]
pub struct PipedProcess(crate::native::Child);

#[cfg(windows)]
impl std::fmt::Debug for PipedProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PipedProcess")
            .field("pid", &self.id())
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
impl PipedProcess {
    pub fn id(&self) -> u32 {
        self.0.id()
    }
    pub fn wait(&mut self) -> std::io::Result<i32> {
        self.0.wait()
    }
    pub fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        self.0.try_wait()
    }
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.0.kill()
    }
    pub fn take_stdin(&mut self) -> Option<Box<dyn std::io::Write + Send>> {
        self.0.take_stdin()
    }
    pub fn take_stdout(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        self.0.take_stdout()
    }
    pub fn take_stderr(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        self.0.take_stderr()
    }
}

/// Quote the Windows CRT argument grammar, including trailing backslashes.
/// MXC passes this directly to CreateProcessW; no command interpreter runs.
#[cfg(windows)]
fn quoted(argument: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            '\\',
            if character == '"' {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        result.push(character);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

#[cfg(windows)]
pub fn spawn_piped(request: &ProcessRequest) -> Result<PipedProcess, SpawnFailure> {
    request.validate()?;
    let executable = request
        .executable
        .to_str()
        .ok_or_else(|| SpawnFailure::refused("sandbox executable must be Unicode", false))?;
    // Windows executable names cannot contain quotes. Refuse instead of
    // allowing CreateProcessW to resolve a different first argument.
    if executable.contains('"') {
        return Err(SpawnFailure::refused(
            "sandbox executable contains a quote",
            false,
        ));
    }
    let line = std::iter::once(executable)
        .chain(request.arguments.iter().map(String::as_str))
        .map(quoted)
        .collect::<Vec<_>>()
        .join(" ");
    let shell = request.shell_request(line);
    shell
        .validate()
        .map_err(|reason| SpawnFailure::refused(reason, false))?;
    crate::native::spawn_piped(&shell).map(PipedProcess)
}

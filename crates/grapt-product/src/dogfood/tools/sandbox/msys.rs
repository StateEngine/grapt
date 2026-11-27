//! Compatibility detection is only a reason to ASK for an unconfined grant.
//! It never authorizes a command or retries an effect that may have executed.
use super::*;

pub(in crate::dogfood::tools) const REASON: &str = "msys runtime needs a named-object namespace";
pub(in crate::dogfood::tools) const SESSION_REASON: &str =
    "session MSYS grant: command and all children run without OS containment";
#[cfg(windows)]
pub(in crate::dogfood::tools) fn failure_note(exit: i32, observed: bool) -> Option<String> {
    let evidence = match (exit, observed) {
        (0, _) => return None,
        (_, true) => "An MSYS program (Git's sh.exe, bash.exe or ssh.exe) was observed inside the Windows sandbox and may have failed to initialize.",
        (-1_073_741_502 | -1_073_741_790, false) => "The contained command returned an initialization/access-denied status (0xC0000142 or 0xC0000022); an MSYS runtime may have failed to initialize.",
        _ => return None,
    };
    Some(format!("{evidence} This does not establish why the command failed. If it needs MSYS, request msys_unconfined=true: Once covers this command tree; Session covers later shell command trees. Either grant removes OS file and network containment. Inspect partial effects before rerunning; no automatic retry occurred."))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dogfood::tools) struct Launch {
    pub program: PathBuf,
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dogfood::tools) struct Mode {
    pub launch: Launch,
    pub whole_command: bool,
    pub session_granted: bool,
}

#[cfg(any(windows, test))]
pub(in crate::dogfood::tools) fn prepare(
    command: &str,
    program: &Path,
    arguments: &[String],
    cmd_shell: bool,
    environment: &super::super::shell_environment::Environment,
) -> Result<Option<Launch>, String> {
    // An explicitly selected MSYS shell is the root of all the approved
    // source. A command-only grant covers its children; Session command-tree
    // consent is applied before this detector by the permission runner.
    if requires_namespace(program) {
        return Ok(Some(Launch {
            program: std::fs::canonicalize(program)
                .map_err(|e| format!("MSYS executable identity unavailable: {e}"))?,
            arguments: arguments.to_vec(),
        }));
    }
    if !cmd_shell {
        return Ok(None);
    }
    if let Some(words) = grapt_permission::command_safety::literal_windows_argv(command) {
        if let Some(path) = words.first().and_then(|name| resolve(name, environment)) {
            if requires_namespace(&path) {
                return Ok(Some(Launch {
                    program: std::fs::canonicalize(path)
                        .map_err(|e| format!("MSYS executable identity unavailable: {e}"))?,
                    arguments: words.into_iter().skip(1).collect(),
                }));
            }
        }
        return Ok(None);
    }
    let reading = grapt_permission::command_safety::simple_commands(command);
    if reading.commands().iter().any(|words| {
        words
            .first()
            .and_then(|name| resolve(name, environment))
            .is_some_and(|path| requires_namespace(&path))
    }) {
        return Err(super::refusal(&super::degraded(REASON),
            "MSYS fallback needs one literal command; split the commands or select its shell explicitly"));
    }
    Ok(None)
}

#[cfg(any(windows, test))]
fn resolve(
    name: &str,
    environment: &super::super::shell_environment::Environment,
) -> Option<PathBuf> {
    if name.len() > 8192 || name.contains('\0') {
        return None;
    }
    let candidate = PathBuf::from(name);
    if candidate.is_absolute() {
        return candidate.is_file().then_some(candidate);
    }
    // Relative names with a directory are left confined. Resolving them against
    // ambient cwd could classify a different executable from the prepared one.
    if candidate.components().count() != 1 {
        return None;
    }
    let path = environment.iter().find_map(|(key, value)| {
        key.eq_ignore_ascii_case(std::ffi::OsStr::new("PATH"))
            .then_some(value)
    })?;
    let filename = if cfg!(windows) && candidate.extension().is_none() {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    std::env::split_paths(path)
        .filter(|path| path.is_absolute())
        .take(128)
        .map(|directory| directory.join(&filename))
        .find(|path| path.is_file())
}

#[cfg(any(windows, test))]
fn requires_namespace(executable: &Path) -> bool {
    let Some(parent) = executable.parent() else {
        return false;
    };
    // These installations share the runtime DLL with their executables. A
    // conservative match can ask unnecessarily; it cannot grant anything.
    if ["msys-2.0.dll", "cygwin1.dll"]
        .iter()
        .any(|name| parent.join(name).is_file())
    {
        return true;
    }
    if !executable
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(std::ffi::OsStr::new("git.exe")))
    {
        return false;
    }
    // Git for Windows' native launcher starts MSYS ssh, Bash and hooks. The
    // grant describes the whole command and its children, not just git.exe.
    parent
        .ancestors()
        .take(4)
        .any(|root| root.join("usr/bin/msys-2.0.dll").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_msys_grant_cannot_unconfine_a_sibling_command() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = std::env::temp_dir().join(format!("grapt-msys-scope-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        let program = root.join("bash.exe");
        std::fs::write(&program, b"fixture; never executed")?;
        std::fs::write(root.join("msys-2.0.dll"), b"fixture; never loaded")?;
        let environment = super::super::super::shell_environment::Environment::parent()?;
        let source = format!("\"{}\" -c true", program.display());
        let native_shell = std::env::current_exe()?;
        let launch = prepare(&source, &native_shell, &[], true, &environment)?
            .ok_or("literal MSYS invocation was not recognized")?;
        assert_eq!(launch.program, std::fs::canonicalize(&program)?);
        assert_eq!(launch.arguments, ["-c", "true"]);
        for extra in [" && echo sibling", " | echo sibling", " > redirected"] {
            assert!(prepare(
                &(source.clone() + extra),
                &native_shell,
                &[],
                true,
                &environment
            )
            .is_err());
        }
        let script = "echo first && echo child";
        let arguments = vec!["-c".into(), script.into()];
        let selected = prepare(script, &program, &arguments, false, &environment)?
            .ok_or("selected MSYS shell lost its invocation")?;
        assert_eq!(selected.arguments, arguments);
        assert!(prepare("echo ordinary", &native_shell, &[], true, &environment)?.is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}

//! Shell environment construction.
//! Copyright 2025 OpenAI.
//! SPDX-License-Identifier: Apache-2.0
//! Modified by Grapt: bounded JSONC config, OS-native values, mandatory final
//! credential filtering, and a prepared snapshot shared by shell/PTY adapters.
use super::*;
use grapt_config::ConfigValue;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

mod agent;
mod config;
#[cfg(test)]
mod git_metadata_tests;
mod hooks;
#[cfg(test)]
mod tests;

const MAX_ENTRIES: usize = 1024;
const MAX_BYTES: usize = 256 * 1024;
const MAX_VALUE_BYTES: usize = 32 * 1024;
const MAX_NAME_BYTES: usize = 256;

#[derive(Default, Clone, Copy)]
enum Inherit {
    #[default]
    All,
    Core,
    None,
}

#[derive(Default)]
struct Policy {
    inherit: Inherit,
    exclude: Vec<regex::Regex>,
    include_only: Vec<regex::Regex>,
    set: BTreeMap<OsString, OsString>,
    credentials: Vec<String>,
}

/// Values stay private and never enter Debug, approval metadata or diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub(in crate::dogfood) struct Environment(
    BTreeMap<OsString, OsString>,
    Option<sandbox::Plan>,
    Option<sandbox::msys::Mode>,
    Vec<grapt_sandbox::WriteTarget>,
);

impl std::fmt::Debug for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellEnvironment")
            .field("names", &self.names())
            .finish_non_exhaustive()
    }
}

/// The surface value StateEngine's seat tooling expects; it is its id, not
/// the product's display name.
const SEAT_SURFACE: &str = "secode"; // grapt-rename: keep

impl Environment {
    /// Stamp runtime identity after user policy and launcher credentials are removed.
    fn with_session(mut self, session: &SessionId) -> Result<Self, String> {
        self.0.retain(|name, _| {
            !["CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID"]
                .iter()
                .any(|key| {
                    name.to_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case(key))
                })
        });
        insert(
            &mut self.0,
            "SECODE_SESSION_ID".into(),
            session.as_str().into(),
            cfg!(windows),
        )?;
        insert(
            &mut self.0,
            "STATEENGINE_SEAT_SURFACE".into(),
            SEAT_SURFACE.into(),
            cfg!(windows),
        )?;
        insert(
            &mut self.0,
            "GIT_TERMINAL_PROMPT".into(),
            "0".into(),
            cfg!(windows),
        )?;
        insert(
            &mut self.0,
            "GIT_EDITOR".into(),
            "true".into(),
            cfg!(windows),
        )?;
        insert(
            &mut self.0,
            "GCM_INTERACTIVE".into(),
            "never".into(),
            cfg!(windows),
        )?;
        insert(&mut self.0, "GIT_ASKPASS".into(), "".into(), cfg!(windows))?;
        insert(&mut self.0, "SSH_ASKPASS".into(), "".into(), cfg!(windows))?;
        insert(
            &mut self.0,
            "SSH_ASKPASS_REQUIRE".into(),
            "never".into(),
            cfg!(windows),
        )?;
        if self.0.len() > MAX_ENTRIES
            || self.0.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() > MAX_BYTES
        {
            return Err("shell environment exceeds 1024 entries or 256 KiB".into());
        }
        Ok(self)
    }

    pub(in crate::dogfood) fn read(
        service: &Path,
        directory: &Path,
        credentials: &[String],
    ) -> Result<Self, String> {
        let sources = super::super::catalog_config::CatalogSources::read(
            directory,
            super::super::catalog_config::CatalogEnvironment::read(service)?,
        )?;
        let mut policy =
            Policy::fold(sources.documents.iter().map(|d| &d.value))?.for_agent(cfg!(windows))?;
        policy.credentials.extend_from_slice(credentials);
        let mut environment = Self::from_parent(std::env::vars_os(), &policy, cfg!(windows))?;
        environment.1 = sandbox::Plan::read(directory, &sources)
            .map_err(|reason| sandbox::refusal(&sandbox::degraded(reason.clone()), &reason))?;
        Ok(environment)
    }

    #[cfg(test)]
    pub(in crate::dogfood) fn parent() -> Result<Self, String> {
        Self::from_parent(std::env::vars_os(), &Policy::default(), cfg!(windows))
    }

    fn from_parent(
        parent: impl IntoIterator<Item = (OsString, OsString)>,
        policy: &Policy,
        windows: bool,
    ) -> Result<Self, String> {
        let mut values = BTreeMap::new();
        let mut bytes = 0usize;
        for (index, (name, value)) in parent.into_iter().enumerate() {
            bytes = bytes.saturating_add(name.len()).saturating_add(value.len());
            if index >= MAX_ENTRIES || bytes > MAX_BYTES {
                return Err("parent shell environment exceeds 1024 entries or 256 KiB".into());
            }
            // A name that cannot be matched safely cannot bypass filtering.
            let Some(text) = name.to_str() else { continue };
            if (windows && text.starts_with('=')) || secret_name(text, &policy.credentials) {
                continue;
            }
            if matches!(policy.inherit, Inherit::None)
                || (matches!(policy.inherit, Inherit::Core) && !core_name(text, windows))
                || policy.exclude.iter().any(|p| p.is_match(text))
            {
                continue;
            }
            insert(&mut values, name, value, windows)?;
        }
        for (name, value) in &policy.set {
            insert(&mut values, name.clone(), value.clone(), windows)?;
        }
        values.retain(|name, _| {
            name.to_str().is_some_and(|text| {
                !secret_name(text, &policy.credentials)
                    && (policy.include_only.is_empty()
                        || policy.include_only.iter().any(|p| p.is_match(text)))
            })
        });
        if values.len() > MAX_ENTRIES
            || values.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() > MAX_BYTES
        {
            return Err("shell environment exceeds 1024 entries or 256 KiB".into());
        }
        Ok(Self(values, None, None, Vec::new()))
    }

    pub(in crate::dogfood::tools) fn sandbox(&self) -> Option<&sandbox::Plan> {
        self.2.is_none().then_some(self.1.as_ref()).flatten()
    }

    pub(in crate::dogfood::tools) fn sandbox_notes(&self, stderr: &[u8]) -> Vec<String> {
        self.sandbox()
            .map(|plan| plan.notes(stderr))
            .unwrap_or_default()
    }

    pub(in crate::dogfood::tools) fn msys_executables(&self) -> &[PathBuf] {
        self.2
            .as_ref()
            .map(|mode| std::slice::from_ref(&mode.launch.program))
            .unwrap_or(&[])
    }

    pub(in crate::dogfood::tools) fn msys_launch(&self) -> Option<&sandbox::msys::Launch> {
        self.2
            .as_ref()
            .filter(|mode| !mode.whole_command)
            .map(|mode| &mode.launch)
    }

    pub(in crate::dogfood::tools) fn intended(&self) -> RealizedEnforcement {
        if let Some(mode) = &self.2 {
            sandbox::degraded(if mode.session_granted {
                sandbox::msys::SESSION_REASON
            } else {
                sandbox::msys::REASON
            })
        } else {
            self.1
                .as_ref()
                .map(sandbox::Plan::intended)
                .unwrap_or(RealizedEnforcement::HarnessOnly)
        }
    }

    pub(in crate::dogfood::tools) fn pending_receipt(&self) -> sandbox::Receipt {
        if self.2.is_none() {
            sandbox::Receipt::pending(self.1.is_some())
        } else {
            sandbox::Receipt {
                enforcement: self.intended(),
                ran: Some(false),
            }
        }
    }

    fn prepare_msys(
        &mut self,
        command: &str,
        program: &Path,
        arguments: &[String],
        cmd_shell: bool,
        session_granted: bool,
        requested: bool,
    ) -> Result<(), String> {
        if requested && (!cfg!(windows) || self.1.is_none()) {
            return Err("MSYS compatibility permission requires an enabled Windows sandbox".into());
        }
        if self
            .1
            .as_ref()
            .is_some_and(sandbox::Plan::git_metadata_read_only)
        {
            if requested {
                return Err("MSYS compatibility cannot preserve read-only Git metadata; use a contained native command or select a profile that permits Git writes".into());
            }
            // A saved compatibility grant cannot remove an active protection.
            self.2 = None;
            return Ok(());
        }
        #[cfg(any(windows, test))]
        if self.1.is_some() {
            self.2 = if session_granted || requested {
                Some(sandbox::msys::Mode {
                    launch: sandbox::msys::Launch {
                        program: program.to_path_buf(),
                        arguments: arguments.to_vec(),
                    },
                    whole_command: true,
                    session_granted,
                })
            } else {
                sandbox::msys::prepare(command, program, arguments, cmd_shell, self)?.map(
                    |launch| sandbox::msys::Mode {
                        launch,
                        whole_command: false,
                        session_granted: false,
                    },
                )
            };
            if self.2.is_some() && !self.3.is_empty() {
                return Err("Sandbox cannot enforce this file grant for an MSYS command".into());
            }
        }
        #[cfg(not(any(windows, test)))]
        let _ = (command, program, arguments, cmd_shell, session_granted);
        Ok(())
    }

    pub(in crate::dogfood::tools) fn write_targets(&self) -> &[grapt_sandbox::WriteTarget] {
        &self.3
    }

    /// Retained terminal records keep names, while the startup owner keeps pins.
    pub(in crate::dogfood::tools) fn release_write_targets(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.3)
            .into_iter()
            .map(|target| target.path().to_path_buf())
            .collect()
    }

    pub(in crate::dogfood::tools) fn with_write_target(
        mut self,
        target: grapt_sandbox::WriteTarget,
    ) -> Result<Self, String> {
        if self.2.is_some() {
            return Err("Sandbox cannot enforce this file grant for an MSYS command".into());
        }
        let plan = self
            .1
            .as_mut()
            .ok_or("Sandbox is not enabled for this command")?;
        if plan.path_count() >= 128 || !self.3.is_empty() {
            return Err("Sandbox retry exceeds its path bound".into());
        }
        plan.add_write_target(target.path());
        self.3.push(target);
        Ok(self)
    }

    pub(in crate::dogfood::tools) fn names(&self) -> Vec<String> {
        self.0
            .keys()
            .filter_map(|k| k.to_str().map(str::to_owned))
            .collect()
    }

    pub(in crate::dogfood) fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.0.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str()))
    }
}

fn insert(
    values: &mut BTreeMap<OsString, OsString>,
    name: OsString,
    value: OsString,
    windows: bool,
) -> Result<(), String> {
    let text = name.to_str().ok_or("shell environment name is not UTF-8")?;
    if text.is_empty()
        || text.len() > MAX_NAME_BYTES
        || text.contains(['=', '\0'])
        || value.len() > MAX_VALUE_BYTES
        || value.as_encoded_bytes().contains(&0)
    {
        return Err("shell environment has an invalid name or value bound".into());
    }
    if windows {
        values.retain(|existing, _| !existing.to_string_lossy().eq_ignore_ascii_case(text));
    }
    values.insert(name, value);
    Ok(())
}

pub(super) fn secret_name(name: &str, credentials: &[String]) -> bool {
    let upper = name.to_ascii_uppercase();
    grapt_service::launcher_environment::is_launcher_identity(OsStr::new(name))
        || ["KEY", "SECRET", "TOKEN", "PASSWORD"]
            .iter()
            .any(|p| upper.contains(p))
        || credentials.iter().any(|p| p.eq_ignore_ascii_case(name))
}

fn core_name(name: &str, windows: bool) -> bool {
    let core: &[&str] = if windows {
        &[
            "PATH",
            "PATHEXT",
            "SHELL",
            "COMSPEC",
            "SYSTEMROOT",
            "WINDIR",
            "SYSTEMDRIVE",
            "USERNAME",
            "USERDOMAIN",
            "USERPROFILE",
            "HOMEDRIVE",
            "HOMEPATH",
            "PROGRAMFILES",
            "PROGRAMFILES(X86)",
            "PROGRAMW6432",
            "PROGRAMDATA",
            "LOCALAPPDATA",
            "APPDATA",
            "TEMP",
            "TMP",
            "TMPDIR",
            "POWERSHELL",
            "PWSH",
        ]
    } else {
        &[
            "PATH", "SHELL", "TMPDIR", "TEMP", "TMP", "HOME", "LANG", "LC_ALL", "LC_CTYPE",
            "LOGNAME", "USER",
        ]
    };
    core.iter().any(|p| p.eq_ignore_ascii_case(name))
}

pub(super) fn prepare_shell(
    root: &Path,
    service: &Path,
    input: &BoundedToolInput,
    credentials: &[String],
) -> Result<PreparedEffect, String> {
    prepare_shell_with_environment(root, input, Environment::read(service, root, credentials)?)
}

pub(super) fn prepare_session(
    root: &Path,
    service: &Path,
    tool: &str,
    input: &BoundedToolInput,
    credentials: &[String],
    session: &SessionId,
    msys_session_granted: bool,
) -> Result<PreparedEffect, String> {
    let requested = match tool {
        SHELL_TOOL => decode_input::<ShellArguments>(input)?
            .msys_unconfined
            .unwrap_or(false),
        terminal::EXEC_COMMAND => decode_input::<terminal::ExecArguments>(input)?
            .msys_unconfined
            .unwrap_or(false),
        _ => false,
    };
    let environment = Environment::read(service, root, credentials)?.with_session(session)?;
    let mut prepared = match tool {
        SHELL_TOOL => prepare_shell_with_environment(root, input, environment),
        terminal::EXEC_COMMAND => terminal::prepare_exec(root, input, environment),
        sandbox_access::TOOL => sandbox_access::prepare(root, input, environment),
        _ => Err("Session shell preparation requires a shell tool".into()),
    }?;
    let environment = match &mut prepared.target {
        EffectTarget::Shell {
            command,
            program,
            environment,
        } => {
            // Legacy Windows execution always uses cmd.exe. The fallback
            // parses its one literal command, not cmd's raw argument framing.
            environment.prepare_msys(
                command,
                program,
                &[],
                true,
                msys_session_granted,
                requested,
            )?;
            prepared.capability = shell_capability(root, command, program, environment);
            environment
        }
        EffectTarget::TerminalExec(spec) => {
            spec.environment.prepare_msys(
                &spec.command,
                &spec.program,
                &spec.arguments,
                spec.cmd_shell,
                msys_session_granted,
                requested,
            )?;
            if let Some(launch) = spec.environment.msys_launch() {
                spec.program = launch.program.clone();
                spec.arguments = launch.arguments.clone();
                spec.cmd_shell = false;
            }
            prepared.capability = terminal::process_capability(root, spec);
            &mut spec.environment
        }
        _ => return Err("Session shell preparation returned another effect".into()),
    };
    if !environment.msys_executables().is_empty() {
        prepared.action = grapt_permission::SANDBOX_MSYS_ACTION.into();
    }
    Ok(prepared)
}

fn prepare_shell_with_environment(
    root: &Path,
    input: &BoundedToolInput,
    environment: Environment,
) -> Result<PreparedEffect, String> {
    let args: ShellArguments = decode_input(input)?;
    if args.command.as_str().trim().is_empty() {
        return Err("shell command must not be empty".into());
    }
    let program = shell_program()?;
    let capability = shell_capability(root, args.command.as_str(), &program, &environment);
    Ok(PreparedEffect {
        target: EffectTarget::Shell {
            command: args.command.as_str().to_owned(),
            program,
            environment,
        },
        capability,
        action: "shell".into(),
        resource: args.command.into_inner(),
    })
}

fn shell_capability(
    root: &Path,
    command: &str,
    program: &Path,
    environment: &Environment,
) -> CapabilityId {
    let root_text = path_text(root);
    let resolved = ResolvedCapability::new(
        EffectClass::ProcessSpawn,
        Some(ProgramIdentity::new(
            PathIdentity::canonical(path_text(
                environment
                    .msys_launch()
                    .map_or(program, |launch| launch.program.as_path()),
            )),
            environment.msys_launch().map_or_else(
                || vec![command.to_owned()],
                |launch| launch.arguments.clone(),
            ),
            PathIdentity::canonical(root_text.clone()),
            environment.names(),
        )),
        environment
            .sandbox()
            .map(sandbox::Plan::filesystem)
            .unwrap_or_else(|| {
                FilesystemAuthority::new(
                    vec![PathIdentity::canonical(root_text.clone())],
                    vec![PathIdentity::canonical(root_text)],
                    protected_paths(root),
                )
            }),
        environment
            .sandbox()
            .map(sandbox::Plan::egress)
            .unwrap_or_default(),
        environment.intended(),
    );
    CapabilityId::derive(&resolved)
}

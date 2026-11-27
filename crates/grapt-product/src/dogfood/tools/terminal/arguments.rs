use super::super::*;

pub(in crate::dogfood::tools) const EXEC_COMMAND: &str = "exec_command";
pub(in crate::dogfood::tools) const WRITE_STDIN: &str = "write_stdin";
const MAX_LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;

tool_arguments! {
    pub(in crate::dogfood::tools) struct ExecArguments => EXEC_COMMAND {
        required cmd: NonEmptyString,
        optional description: ToolDescription,
        optional workdir: NonEmptyString,
        optional shell: NonEmptyString,
        optional login: bool,
        optional tty: bool,
        optional yield_time_ms: BoundedU64<250, 30_000>,
        optional timeout_ms: BoundedU64<1, MAX_LIFETIME_MS>,
        optional max_output_tokens: BoundedU64<1, 16_384>,
        optional msys_unconfined: bool,
    }
}

tool_arguments! {
    pub(in crate::dogfood::tools) struct StdinArguments => WRITE_STDIN {
        required session_id: NonEmptyString,
        optional chars: String,
        optional yield_time_ms: BoundedU64<250, 300_000>,
        optional max_output_tokens: BoundedU64<1, 16_384>,
        optional cursor: OutputCursor,
    }
}

/// The output_cursor the most recent terminal response returned. Opaque: it
/// is echoed back unchanged to continue past observed output, never computed
/// from page lengths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dogfood) struct OutputCursor(u64);

impl OutputCursor {
    const fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for OutputCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self(u64::deserialize(deserializer)?))
    }
}

impl ProviderFieldSchema for OutputCursor {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({"type": "integer", "minimum": 0,
            "maximum": JSON_SAFE_INTEGER_MAX,
            "description": "Opaque. Pass back exactly the output_cursor from the most recent response to continue; do not add page lengths or compute it."})
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dogfood::tools) struct ExecSpec {
    pub origin: Option<super::recovery::Origin>,
    pub grant: Option<super::GrantLease>,
    pub command: String,
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub environment: shell_environment::Environment,
    pub cmd_shell: bool,
    pub login: bool,
    pub cwd: PathBuf,
    pub tty: bool,
    pub wait_ms: u64,
    pub timeout_ms: u64,
    pub page_bytes: usize,
}

#[derive(Debug, Clone)]
pub(in crate::dogfood::tools) struct StdinSpec {
    pub operation: grapt_types::OperationId,
    pub chars: String,
    pub wait_ms: u64,
    pub page_bytes: usize,
    pub cursor: Option<u64>,
    /// The input line this write was decided against; `None` until decided.
    pub line: Option<InputLine>,
}

/// The unfinished terminal input line a write was decided against, and the
/// line it leaves once delivered. `None` is input too long or too uncertain
/// to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dogfood::tools) struct InputLine {
    pub before: Option<String>,
    pub after: Option<String>,
}

fn page_bytes(tokens: Option<BoundedU64<1, 16_384>>) -> usize {
    (tokens.map(BoundedU64::get).unwrap_or(10_000) as usize * 4).clamp(4, 64 * 1024)
}

pub(super) fn decode_exec(
    root: &Path,
    input: &BoundedToolInput,
    environment: shell_environment::Environment,
) -> Result<ExecSpec, String> {
    let args: ExecArguments = decode_input(input)?;
    let _ = args.description;
    if args.cmd.as_str().contains('\0') {
        return Err("exec_command cmd must not contain NUL bytes".into());
    }
    if args.cmd.as_str().trim().is_empty() || args.cmd.as_str().len() > 64 * 1024 {
        return Err("exec_command cmd must contain 1..65536 bytes of shell source".into());
    }
    let cwd = match args.workdir {
        Some(path) => resolve_existing(root, path.as_str(), false)?,
        None => std::fs::canonicalize(root)
            .map_err(|error| format!("workspace is unreadable: {error}"))?,
    };
    if !cwd.is_dir() {
        return Err("exec_command workdir must be a workspace directory".into());
    }
    let login = args.login.unwrap_or(false);
    let (program, arguments, cmd_shell) = super::command::resolve(
        args.shell.as_ref().map(NonEmptyString::as_str),
        args.cmd.as_str(),
        login,
        &environment,
    )?;
    Ok(ExecSpec {
        origin: None,
        grant: None,
        command: args.cmd.into_inner(),
        program,
        arguments,
        environment,
        cmd_shell,
        login,
        cwd,
        tty: args.tty.unwrap_or(false),
        wait_ms: args.yield_time_ms.map(BoundedU64::get).unwrap_or(10_000),
        timeout_ms: args
            .timeout_ms
            .map(BoundedU64::get)
            .unwrap_or(MAX_LIFETIME_MS),
        page_bytes: page_bytes(args.max_output_tokens),
    })
}

pub(super) fn decode_stdin(input: &BoundedToolInput) -> Result<StdinSpec, String> {
    let args: StdinArguments = decode_input(input)?;
    let operation = grapt_types::OperationId::new(args.session_id.into_inner())
        .ok_or("invalid terminal session_id")?;
    let chars = args.chars.unwrap_or_default();
    if chars.len() > 64 * 1024 {
        return Err("write_stdin chars exceeds 64 KiB".into());
    }
    let wait_ms = match (chars.is_empty(), args.yield_time_ms.map(BoundedU64::get)) {
        (true, value) => value.unwrap_or(5_000).clamp(5_000, 300_000),
        (false, value) => value.unwrap_or(250).clamp(250, 30_000),
    };
    Ok(StdinSpec {
        operation,
        chars,
        wait_ms,
        page_bytes: page_bytes(args.max_output_tokens),
        cursor: args.cursor.map(OutputCursor::get),
        line: None,
    })
}

pub(in crate::dogfood::tools) fn prepare_exec(
    root: &Path,
    input: &BoundedToolInput,
    environment: shell_environment::Environment,
) -> Result<PreparedEffect, String> {
    let spec = decode_exec(root, input, environment)?;
    let resource = permission_resource(&spec);
    Ok(PreparedEffect {
        capability: process_capability(root, &spec),
        target: EffectTarget::TerminalExec(spec),
        action: "shell".into(),
        resource,
    })
}

pub(in crate::dogfood::tools) fn permission_resource(spec: &ExecSpec) -> String {
    format!(
        "{}\nworkdir: {}\nshell: {}\nlogin: {}\nterminal: {}",
        spec.command,
        path_text(&spec.cwd),
        path_text(&spec.program),
        spec.login,
        spec.tty
    )
}

pub(in crate::dogfood::tools) fn process_capability(root: &Path, spec: &ExecSpec) -> CapabilityId {
    let mut env = spec.environment.names();
    if spec.tty {
        env.push("TERM".into());
    }
    CapabilityId::derive(&ResolvedCapability::new(
        EffectClass::ProcessSpawn,
        Some(ProgramIdentity::new(
            PathIdentity::canonical(path_text(&spec.program)),
            spec.arguments.clone(),
            PathIdentity::canonical(path_text(&spec.cwd)),
            env,
        )),
        spec.environment
            .sandbox()
            .map(sandbox::Plan::filesystem)
            .unwrap_or_else(|| {
                FilesystemAuthority::new(
                    vec![PathIdentity::canonical(path_text(root))],
                    vec![PathIdentity::canonical(path_text(root))],
                    protected_paths(root),
                )
            }),
        spec.environment
            .sandbox()
            .map(sandbox::Plan::egress)
            .unwrap_or_default(),
        spec.environment.intended(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_integer_bounds_do_not_narrow_returned_terminal_cursors() -> Result<(), String> {
        for cursor in [0, 1_u64 << 53, u64::MAX] {
            let encoded = serde_json::to_vec(&serde_json::json!({
                "session_id": "op_schema_cursor", "cursor": cursor,
            }))
            .map_err(|error| error.to_string())?;
            let input = BoundedToolInput::new(encoded).map_err(|error| error.to_string())?;
            assert_eq!(decode_stdin(&input)?.cursor, Some(cursor));
        }
        Ok(())
    }
}

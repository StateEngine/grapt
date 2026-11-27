//! Effect-free selection of a volatile run, before installation or profile work.
//!
//! select ephemeral persistence policy before session initialization.
//! Copyright 2025 OpenAI. License/NOTICE: licenses/openai-codex/.

use super::{CliCommand, CommandError, Parsed};

/// Called before bundled dispatch, installation preparation and Windows redirects.
/// Literal prompt/option values and words after `--` are not mode switches.
pub fn parse_early(arguments: &[String]) -> Option<Result<CliCommand, CommandError>> {
    let mut values = arguments.iter();
    while let Some(value) = values.next() {
        match value.as_str() {
            "--" => break,
            "--ephemeral" => {
                return Some(if arguments.first().is_some_and(|value| value == "run") {
                    CliCommand::parse(arguments.iter().cloned())
                } else {
                    Err(CommandError::EphemeralTarget)
                });
            }
            "--service" | "--product" | "--remote" | "--credential" | "--session" | "-s"
            | "--project" | "--prompt" | "--title" | "--format" | "--reasoning-effort"
            | "--model" | "-m" | "--agent" | "--max-steps" | "--worktree" | "--profile"
            | "--setup" | "--image" | "-i" => {
                let _value = values.next();
            }
            _ => {}
        }
    }
    None
}

pub(super) fn validate(parsed: &Parsed) -> Result<(), CommandError> {
    if parsed.ephemeral
        && (!parsed.embedded
            || parsed.remote.is_some()
            || parsed.credential_file.is_some()
            || parsed.service_root.is_none()
            || parsed.product_binary.is_none()
            || parsed.session.is_some()
            || parsed.resume
            || parsed.project.is_some()
            || parsed.fork
            || parsed.background
            || parsed.resident
            || parsed.worktree.is_some())
    {
        return Err(CommandError::EphemeralTarget);
    }
    Ok(())
}

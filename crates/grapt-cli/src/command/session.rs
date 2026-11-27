//! Session metadata uses the existing title rules, without submitting a prompt.
//! Rename normalization.
// Copyright 2025 OpenAI. Modified to use Grapt's existing title contract.
use super::*;

pub(super) fn rename(arguments: &[String]) -> Result<CliCommand, CommandError> {
    let mut parsed = parse_options(arguments, true)?;
    let words = std::mem::take(&mut parsed.message);
    if words.len() != 2
        || parsed.embedded
        || parsed.product_binary.is_some()
        || parsed.session.is_some()
        || parsed.resume
        || parsed.new_session
        || parsed.project.is_some()
        || parsed.prompt.is_some()
        || parsed.stdin
        || parsed.json
        || parsed.jsonl
        || parsed.has_run_only()
    {
        return Err(CommandError::RenameArguments);
    }
    let session = words[0].parse().map_err(|_| CommandError::InvalidSession)?;
    let title = validate_title(&words[1])?;
    let connection = if parsed.service_root.is_none()
        && parsed.remote.is_none()
        && parsed.credential_file.is_none()
    {
        None
    } else {
        Some(connection(&parsed)?)
    };
    Ok(CliCommand::SessionRename {
        connection,
        session,
        title,
    })
}

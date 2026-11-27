//! Ask the product's native agent loader rather than copying its files.
use super::{display, files, home};
use crate::{ApiClient, CliLimits, ServiceDiscovery};
use std::path::Path;

pub async fn report(
    root: &Path,
    directory: &str,
    limits: CliLimits,
) -> Result<Vec<String>, String> {
    let mut sources = Vec::new();
    for (base, layer) in [
        (
            Path::new(directory).join(".claude/agents"),
            "claude-project",
        ),
        (home()?.join(".claude/agents"), "claude-user"),
    ] {
        for path in files::scan(&base)? {
            if path.extension().is_some_and(|extension| extension == "md") {
                sources.push((path, layer));
            }
        }
    }
    if sources.is_empty() {
        return Ok(vec!["No Claude agent files found.".into()]);
    }
    let service = ServiceDiscovery::try_new(root.to_path_buf(), env!("CARGO_PKG_VERSION"))
        .map_err(|error| error.to_string())?
        .discover()
        .await
        .map_err(|error| error.to_string())?;
    let client = ApiClient::from_discovered(&service, limits).map_err(|error| error.to_string())?;
    let roster = client
        .get_value_query("api/agent", &[("location[directory]", directory)])
        .await
        .map_err(|error| error.to_string())?;
    let rows = roster
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or("agent roster has no rows")?;
    let mut report = Vec::new();
    for (source, layer) in sources {
        let bytes = files::read(&source)?.ok_or("Claude agent disappeared")?;
        let text = std::str::from_utf8(&bytes).map_err(|_| "Claude agent is not UTF-8")?;
        let name = super::metadata::agent_name(text)?;
        let offered = rows
            .iter()
            .find(|row| row.get("name").and_then(serde_json::Value::as_str) == Some(name.as_str()));
        match offered.and_then(|row| row.get("source")).and_then(serde_json::Value::as_str) {
            Some(winner) if winner == layer => report.push(format!(
                "already used: {} (native Claude agent loader)", display(&source))),
            Some(_) => report.push(format!(
                "shadowed by another agent declaration: {}; no agent files copied", display(&source))),
            None => return Err(format!(
                "{} is not offered with a verified source by this server's native agent loader; no agent files copied", display(&source))),
        }
    }
    Ok(report)
}

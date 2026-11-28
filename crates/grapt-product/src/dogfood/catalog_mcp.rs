//! Server prompt provenance is appended after the plugin/file command fold.
use super::{connections, mcp::content::prompt_command_name, session_config, ServerApplication};
use grapt_mcp::content::CatalogKind;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn scope(registry_key: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, registry_key.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) async fn append(
    application: &ServerApplication,
    selection: &session_config::catalog::Selection,
    rows: &mut Vec<Value>,
    mut warnings: Vec<String>,
) -> Vec<String> {
    let base = json!({"data":&*rows,"location":selection.location.wire(),"warnings":[],"mcp_scope":scope(&selection.registry_key)});
    let available = grapt_api::u3a::U3A_PRODUCTION_LIMITS
        .page_bytes
        .saturating_sub(base.to_string().len());
    let warning_budget = available.min(65_536);
    let mut remaining = available.saturating_sub(warning_budget);
    let snapshot =
        match connections::McpSnapshot::read(&selection.sources, &selection.location.directory) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                warnings.push(warning(format!(
                    "MCP prompt discovery unavailable: {error}"
                )));
                return fit_warnings(warnings, warning_budget);
            }
        };
    if let Err(error) = snapshot
        .prepare_roots(
            &application.mcp,
            &selection.registry_key,
            selection.roots_revision,
        )
        .await
    {
        warnings.push(warning(format!("MCP roots unavailable: {error}")));
        return fit_warnings(warnings, warning_budget);
    }
    let mut jobs = tokio::task::JoinSet::new();
    for (name, server) in application
        .mcp
        .content_servers(
            &selection.registry_key,
            &selection.location.directory,
            &snapshot.declared,
        )
        .await
    {
        match server {
            Ok(server) if server.prompts => {
                jobs.spawn(async move { (name, server.catalog(CatalogKind::Prompts).await) });
            }
            Ok(_) => {}
            Err(error) => warnings.push(warning(error)),
        }
    }
    let mut catalogs = Vec::new();
    while let Some(result) = jobs.join_next().await {
        match result {
            Ok(catalog) => catalogs.push(catalog),
            Err(error) => warnings.push(warning(format!(
                "MCP prompt discovery interrupted: {error}"
            ))),
        }
    }
    catalogs.sort_by(|left, right| left.0.cmp(&right.0));
    let local: BTreeSet<String> = rows
        .iter()
        .filter_map(|row| row["command"]["name"].as_str().map(str::to_owned))
        .collect();
    let mut claims = BTreeMap::<String, usize>::new();
    let mut candidates = Vec::new();
    let mut overflow = false;
    let capacity = 256usize.saturating_sub(rows.len());
    for (server, prompts) in catalogs {
        let prompts = match prompts {
            Ok(prompts) => prompts,
            Err(error) => {
                warnings.push(warning(format!(
                    "MCP {} prompts unavailable: {error}",
                    server.escape_debug()
                )));
                continue;
            }
        };
        if server.len() > 256 {
            warnings.push("MCP server name exceeds the native command bound".into());
            continue;
        }
        for prompt in prompts {
            let Some(raw_name) = prompt["name"].as_str() else {
                continue;
            };
            let name = prompt_command_name(&server, raw_name);
            *claims.entry(name.clone()).or_default() += 1;
            if candidates.len() >= capacity {
                overflow = true;
                continue;
            }
            let args = prompt["arguments"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default();
            let hints: Vec<String> = args
                .iter()
                .map(|arg| {
                    let name = arg["name"].as_str().unwrap_or_default();
                    if arg["required"] == true {
                        format!("<{name}>")
                    } else {
                        format!("[{name}]")
                    }
                })
                .collect();
            let description = format!(
                "{} {}",
                prompt["description"].as_str().unwrap_or("MCP prompt"),
                hints.join(" ")
            );
            candidates.push(json!({
                "command":{"name":name,"template":"","description":description.trim()},
                "origin":"mcp", "source":format!("MCP {}",server.escape_debug()),
                "mcp":{"server":server,"name":raw_name,"arguments":args},
            }));
        }
    }
    let mut collisions = 0;
    for candidate in candidates {
        let name = candidate["command"]["name"].as_str().unwrap_or_default();
        if local.contains(name) || claims.get(name).copied().unwrap_or_default() != 1 {
            collisions += 1;
        } else {
            let bytes = candidate.to_string().len().saturating_add(1);
            if bytes > remaining {
                overflow = true;
                continue;
            }
            remaining -= bytes;
            rows.push(candidate);
        }
    }
    if collisions > 0 {
        warnings.push(format!("{collisions} MCP prompt commands were withheld because their names collide; rename the server or prompt."));
    }
    if overflow {
        warnings.push("Command catalog exceeds its aggregate entry or response-size bound; later MCP commands were withheld.".into());
    }
    if warnings.len() > 64 {
        let omitted = warnings.len() - 63;
        warnings.truncate(63);
        warnings.push(format!(
            "{omitted} additional MCP discovery warnings; inspect /mcp for server status."
        ));
    }
    fit_warnings(warnings, warning_budget)
}

fn fit_warnings(warnings: Vec<String>, bytes: usize) -> Vec<String> {
    let summary = "More MCP discovery warnings were withheld by the response-size limit.";
    let summary_bytes = json!(summary).to_string().len().saturating_add(1);
    if bytes < summary_bytes {
        return Vec::new();
    }
    let mut remaining = bytes - summary_bytes;
    let mut result = Vec::new();
    let mut omitted = false;
    for warning in warnings {
        let size = json!(warning).to_string().len().saturating_add(1);
        if size > remaining {
            omitted = true;
            continue;
        }
        remaining -= size;
        result.push(warning);
    }
    if omitted {
        result.push(summary.into());
    }
    result
}

fn warning(mut value: String) -> String {
    if value.len() > 4096 {
        let mut end = 4093;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        value.push('…');
    }
    value
}

//! Import MCP server definitions from existing agent configurations.
//!
//! Definitions are read from the source tool's files, checked against the
//! configuration-only route (which starts no peer), and written one by one
//! through the existing `mcp add` writer. Secrets travel in the write
//! payload and nowhere else: no report line ever carries an environment
//! value, a header value or a full URL. The file reads and entry parsing
//! live in `import_source`.
use crate::command::import::{ImportCommand, ImportSource};
use crate::{ApiClient, CliLimits, ExitClass, ServiceDiscovery};
use grapt_api::u3a::McpConfigurationRequestDto;
use std::collections::BTreeSet;

use super::import_source::{read_source, Imported};

#[cfg(test)]
use super::import_source::remote_entry;

type Failure = (ExitClass, String);

pub(super) async fn execute(command: ImportCommand, limits: CliLimits) -> u8 {
    let result = tokio::select! {
        result = crate::migration::run(command, limits) => result,
        _signal = super::termination_signal() => return ExitClass::Cancelled.code(),
    };
    let (class, rows) = match result {
        Ok(rows) => (ExitClass::Success, rows),
        Err((class, message)) => (class, vec![format!("grapt: {message}")]),
    };
    for row in rows {
        let diagnostic = class != ExitClass::Success || row.starts_with("refused ");
        let emitted = tokio::select! {
            result = super::emit(row, diagnostic, limits) => result,
            _signal = super::termination_signal() => return ExitClass::Cancelled.code(),
        };
        if emitted.is_err() {
            return ExitClass::Output.code();
        }
    }
    class.code()
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) async fn report(
    command: ImportCommand,
    limits: CliLimits,
) -> Result<Vec<String>, Failure> {
    let ImportCommand {
        source,
        service_root,
        directory,
        apply,
        ..
    } = command;
    let service_root =
        service_root.ok_or_else(|| (ExitClass::Usage, "mcp requires --service".to_owned()))?;
    let project = directory.clone().or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    });
    let read = tokio::task::spawn_blocking(move || read_source(source, project.as_deref()))
        .await
        .map_err(|_| {
            (
                ExitClass::Output,
                "the import reader could not finish".to_owned(),
            )
        })?
        .map_err(|reason| (ExitClass::Usage, reason))?;
    let mut entries = read.entries;
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let plan = Plan {
        entries,
        refused: read.refused,
    };
    let mut rows: Vec<_> = plan
        .refused
        .iter()
        .map(|(name, reason)| refused_line(name, reason))
        .collect();
    if plan.entries.is_empty() {
        rows.push(source_finding(source, !plan.refused.is_empty()));
        return Ok(rows);
    }
    let discovery = ServiceDiscovery::try_new(service_root, env!("CARGO_PKG_VERSION"))
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
    let service = discovery
        .discover()
        .await
        .map_err(|error| (ExitClass::Discovery, error.to_string()))?;
    let client = ApiClient::from_discovered(&service, limits)
        .map_err(|error| (ExitClass::Protocol, error.to_string()))?;
    let existing = client
        .mcp_configuration_list(directory.as_deref())
        .await
        .map_err(|error| {
            (
                error.exit_class(),
                error.to_string().escape_debug().to_string(),
            )
        })?;
    let existing_names: BTreeSet<String> = existing
        .data()
        .get()
        .as_slice()
        .iter()
        .map(|row| row.name().get().as_str().to_owned())
        .collect();
    let adds = plan.adds(&existing_names);
    let present = plan.entries.len() - adds.len();
    rows.insert(0, plan_header(source, adds.len(), present, apply));
    for row in &adds {
        if apply {
            let request = McpConfigurationRequestDto::Add {
                config: serde_json::from_value(row.config.clone()).map_err(|_| {
                    (
                        ExitClass::Protocol,
                        "the import configuration is invalid".to_owned(),
                    )
                })?,
            };
            client
                .mcp_configuration(&row.name, directory.as_deref(), &request)
                .await
                .map_err(|error| {
                    (
                        error.exit_class(),
                        format!(
                            "add {} failed: {}; earlier servers may already have been added",
                            row.name.escape_debug(),
                            error
                        ),
                    )
                })?;
        }
        rows.push(format!(
            "{} {} ({}, {} source)",
            if apply { "added" } else { "would add" },
            row.name.escape_debug(),
            row.transport,
            row.source_scope
        ));
    }
    if present > 0 {
        rows.push(format!("skipped {present} already present"));
    }
    Ok(rows)
}

fn refused_line(name: &str, reason: &str) -> String {
    format!("refused {}: {reason}", name.escape_debug())
}

fn source_finding(source: ImportSource, refused_only: bool) -> String {
    let wherefrom = match source {
        ImportSource::Claude => "~/.claude.json or the project .mcp.json",
        ImportSource::Codex => "~/.codex/config.toml",
    };
    if refused_only {
        format!(
            "import mcp from {}: no importable servers in {wherefrom}",
            source.label()
        )
    } else {
        format!("No MCP servers found in {wherefrom}.")
    }
}

struct Plan {
    entries: Vec<Imported>,
    refused: Vec<(String, String)>,
}

impl Plan {
    fn adds(&self, existing: &BTreeSet<String>) -> Vec<Imported> {
        self.entries
            .iter()
            .filter(|entry| !existing.contains(&entry.name))
            .cloned()
            .collect()
    }
}

fn plan_header(source: ImportSource, adds: usize, present: usize, apply: bool) -> String {
    format!(
        "import mcp from {} ({}): {adds} to add, {present} already present",
        source.label(),
        if apply { "apply" } else { "dry run" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_sorts_and_skips_names_the_service_already_declares() {
        let entry = |name: &str| Imported {
            name: name.to_owned(),
            transport: "local",
            source_scope: "user",
            config: serde_json::json!({}),
        };
        let plan = Plan {
            entries: {
                let mut entries = vec![entry("zeta"), entry("alpha"), entry("mid")];
                entries.sort_by(|left, right| left.name.cmp(&right.name));
                entries
            },
            refused: Vec::new(),
        };
        let existing: BTreeSet<String> = ["mid", "ghost"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        let adds: Vec<String> = plan
            .adds(&existing)
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(adds, vec!["alpha", "zeta"]);
        assert_eq!(
            plan_header(ImportSource::Claude, 2, 1, false),
            "import mcp from claude (dry run): 2 to add, 1 already present"
        );
        assert_eq!(
            plan_header(ImportSource::Codex, 2, 1, true),
            "import mcp from codex (apply): 2 to add, 1 already present"
        );
    }

    #[test]
    fn report_lines_escape_names_and_carry_no_url_or_secret_material(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fixture = serde_json::json!({
            "type": "remote",
            "url": "https://host.example/mcp?api_key=leak",
            "headers": {"Authorization": "Bearer secret"},
        });
        let object = fixture.as_object().ok_or("fixture must be an object")?;
        match remote_entry("bad\u{1b}name", object, "user") {
            Err(refused) => assert_eq!(refused, "server name is invalid"),
            Ok(_) => panic!("an invalid name must be refused"),
        }
        let entry = remote_entry("docs", object, "user")?;
        let rendered = format!(
            "added {} ({}, {} source)",
            entry.name.escape_debug(),
            entry.transport,
            entry.source_scope
        );
        assert!(!rendered.contains("secret"), "{rendered}");
        assert!(!rendered.contains("http"), "{rendered}");
        assert_eq!(rendered, "added docs (remote, user source)");
        assert_eq!(
            refused_line("bad\u{1b}name", "transport is not supported"),
            "refused bad\\u{1b}name: transport is not supported"
        );
        Ok(())
    }
}

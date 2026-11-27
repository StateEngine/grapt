// Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// at 2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8. Modified by Grapt:
// raw public DTO content in source order, including reasoning and structured records.
use grapt_api::u3a::{SessionMessageAssistantDtoContentItem as Item, SessionMessageInfoDto as Row};
use grapt_client::{client::SessionExport, wire::Spelled};
use std::io::{self, Write};

pub(super) fn render(history: &SessionExport, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "# Session {}\n", history.session.spelled())?;
    for row in &history.data {
        match row {
            Row::User(user) => writeln!(out, "## User\n\n{}\n", user.text().get().as_str())?,
            Row::Synthetic(prompt) => writeln!(
                out,
                "## Automated prompt\n\n{}\n",
                prompt.text().get().as_str()
            )?,
            Row::System(system) => {
                writeln!(out, "## System\n\n{}\n", system.text().get().as_str())?
            }
            Row::Assistant(assistant) => {
                writeln!(out, "## Assistant\n")?;
                for item in assistant.content().get().as_slice() {
                    match item {
                        Item::Text(text) => writeln!(out, "{}\n", text.text().get().as_str())?,
                        Item::Reasoning(text) => {
                            writeln!(out, "### Reasoning\n\n{}\n", text.text().get().as_str())?
                        }
                        Item::Tool(_) => {
                            writeln!(out, "### Tool\n\n```json")?;
                            serde_json::to_writer_pretty(&mut *out, item)
                                .map_err(io::Error::other)?;
                            writeln!(out, "\n```\n")?;
                        }
                    }
                }
                if let Some(error) = assistant.error().as_present() {
                    writeln!(out, "### Error\n\n```json")?;
                    serde_json::to_writer_pretty(&mut *out, error).map_err(io::Error::other)?;
                    writeln!(out, "\n```\n")?;
                }
            }
            // No public row is silently discarded just because the TUI cannot draw it.
            _ => {
                writeln!(out, "## Session record\n\n```json")?;
                serde_json::to_writer_pretty(&mut *out, row).map_err(io::Error::other)?;
                writeln!(out, "\n```\n")?;
            }
        }
    }
    Ok(())
}

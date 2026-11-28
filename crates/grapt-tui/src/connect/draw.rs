use super::*;
use grapt_tui_transcript::markdown::literal;
use ratatui::{
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn draw(connect: &mut Connect, frame: &mut Frame<'_>, area: Rect, palette: Palette) {
    let entries = connect.entries();
    let Some(pane) = &mut connect.pane else {
        return;
    };
    frame.render_widget(Clear, area);
    let title = pane
        .provider
        .as_ref()
        .map(|p| format!("Connect {}", literal(p.name().get().as_str())))
        .unwrap_or_else(|| "Connect a provider".into());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(palette.normal().bg(palette.panel));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let footer = "↑↓ choose · Enter continue · Esc back/cancel";
    let mut lines = Vec::new();
    let mut options = Vec::new();
    match &pane.screen {
        Screen::Providers => {
            lines.push(Line::from(format!("Search: {}", literal(&pane.input))));
            for index in entries {
                let provider = &connect.providers[index];
                let labels: Vec<_> = provider
                    .connections()
                    .get()
                    .as_slice()
                    .iter()
                    .map(|c| match c {
                        ConnectionInfoDto::Credential(c) => literal(c.label().get().as_str()),
                        ConnectionInfoDto::Env(_) => "Connected".into(),
                    })
                    .collect();
                options.push(format!(
                    "{}{}{}",
                    if labels.is_empty() { "  " } else { "✓ " },
                    literal(provider.name().get().as_str()),
                    if labels.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", labels.join(", "))
                    }
                ));
            }
            if options.is_empty() {
                lines.push(Line::from(
                    "No providers found. Esc closes; Ctrl+A retries.",
                ));
            }
        }
        Screen::Choices => {
            options = pane
                .choices()
                .into_iter()
                .map(|choice| match choice {
                    Choice::AzureCli => "Add connection · Microsoft Entra ID (Azure CLI)".into(),
                    Choice::GoogleAdc => {
                        "Add connection · Google application-default credentials".into()
                    }
                    Choice::AwsChain => "Add connection · AWS credential chain".into(),
                    Choice::Key(label) => format!("Add connection · {}", literal(&label)),
                    Choice::OAuth(method) => literal(method.label().get().as_str()),
                    Choice::Rename(_, label) => format!("Rename {}", literal(&label)),
                    Choice::Remove(_, label) => format!("Disconnect {}", literal(&label)),
                })
                .collect();
            if options.is_empty() {
                lines.push(Line::from(
                    "No connection methods are available. Esc returns to providers.",
                ));
            }
        }
        Screen::Key => {
            lines.push(Line::from(if pane.azure_cli {
                "Azure CLI supplies renewable credentials. Run az login as the service user first."
            } else if pane.google_adc {
                "Use your Google application-default credentials with this Vertex project and location."
            } else if pane.aws_chain {
                "Choose an AWS profile, or leave blank for the default credential chain."
            } else if pane.is_vertex() {
                "Paste your service-account JSON key. Grapt checks it before saving."
            } else {
                "Enter your API key. Grapt checks it before saving."
            }));
            if !pane.azure_cli && !pane.google_adc {
                lines.push(Line::from(format!(
                    "{}{}: {}",
                    if pane.key_field == 0 { "› " } else { "  " },
                    if pane.aws_chain {
                        "AWS profile (optional)"
                    } else if pane.is_vertex() {
                        "Service-account JSON"
                    } else {
                        "API key"
                    },
                    if pane.aws_chain {
                        literal(&pane.input)
                    } else {
                        "•".repeat(pane.input.chars().count().min(48))
                    }
                )));
            }
            lines.push(Line::from(format!(
                "{}Label (optional): {}",
                if pane.key_field == 1 { "› " } else { "  " },
                literal(&pane.label)
            )));
            if pane.endpoint_inputs() || pane.databricks() {
                for (index, name, value) in [
                    (
                        2,
                        if pane.databricks() {
                            "Workspace URL"
                        } else {
                            "API base URL"
                        },
                        &pane.endpoint,
                    ),
                    (3, "Model ID", &pane.model),
                    (4, "Context window (tokens)", &pane.context),
                ] {
                    lines.push(Line::from(format!(
                        "{}{}: {}",
                        if pane.key_field == index {
                            "› "
                        } else {
                            "  "
                        },
                        name,
                        literal(value)
                    )));
                }
                if pane.databricks() {
                    lines.push(Line::from("Use your HTTPS workspace root URL and model service name. Connecting sends a small test request."));
                } else if let Some(provider) = pane.endpoint_provider() {
                    lines.push(Line::from(provider.hint));
                    lines.push(Line::from(
                        "Connecting sends a small test request before saving.",
                    ));
                } else {
                    lines.push(Line::from(
                        "Use an HTTPS API base or http://127.0.0.1:<port>/v1.",
                    ));
                    lines.push(Line::from("Leave the model empty to use every listed model that can call tools and return text. Local HTTP servers may leave the key empty. Connecting sends a small test request."));
                }
            }
            if pane.azure() {
                for (index, name, value) in [
                    (2, "Azure resource endpoint", &pane.endpoint),
                    (3, "Catalog model ID", &pane.model),
                    (4, "Deployment name", &pane.deployment),
                ] {
                    lines.push(Line::from(format!(
                        "{}{}: {}",
                        if pane.key_field == index {
                            "› "
                        } else {
                            "  "
                        },
                        name,
                        literal(value)
                    )));
                }
                lines.push(Line::from(
                    "Connect sends a small Responses request to your deployment.",
                ));
            }
            if pane.bedrock() {
                for (index, name, value) in [
                    (2, "Endpoint override (optional)", &pane.endpoint),
                    (3, "Model ID", &pane.model),
                    (4, "Region", &pane.region),
                ] {
                    lines.push(Line::from(format!(
                        "{}{}: {}",
                        if pane.key_field == index {
                            "› "
                        } else {
                            "  "
                        },
                        name,
                        literal(value)
                    )));
                }
            }
            if pane.is_cloud() {
                for (index, name, value) in [
                    (
                        2,
                        if pane.is_watsonx() {
                            "IBM project ID"
                        } else {
                            "Google Cloud project"
                        },
                        &pane.project,
                    ),
                    (
                        3,
                        if pane.is_watsonx() {
                            "IBM region"
                        } else {
                            "Location (global or region)"
                        },
                        &pane.location,
                    ),
                    (
                        4,
                        if pane.is_watsonx() {
                            "Model ID"
                        } else {
                            "Gemini model ID"
                        },
                        &pane.model,
                    ),
                ] {
                    lines.push(Line::from(format!(
                        "{}{}: {}",
                        if pane.key_field == index {
                            "› "
                        } else {
                            "  "
                        },
                        name,
                        literal(value)
                    )));
                }
            }
            lines.push(Line::from("Tab switches fields · Enter saves"));
        }
        Screen::Gateway(draft) => draft.draw(&mut lines),
        Screen::Prompt { method, index, .. } => {
            if let Some(prompt) = method
                .prompts()
                .as_present()
                .and_then(|p| p.as_slice().get(*index))
            {
                match prompt {
                    IntegrationPromptDto::Text(t) => {
                        lines.push(Line::from(literal(t.message().get().as_str())));
                        lines.push(Line::from(format!("› {}", literal(&pane.input))));
                    }
                    IntegrationPromptDto::Select(s) => {
                        lines.push(Line::from(literal(s.message().get().as_str())));
                        options = s
                            .options()
                            .get()
                            .as_slice()
                            .iter()
                            .map(|o| literal(o.label().get().as_str()))
                            .collect();
                    }
                }
            }
        }
        Screen::Attempt(attempt) => {
            lines.push(Line::from(literal(attempt.instructions().get().as_str())));
            lines.push(Line::from(""));
            lines.push(
                Line::from(grapt_client::cells::cut(
                    &literal(attempt.url().get().as_str()),
                    usize::from(inner.width),
                ))
                .style(palette.normal().fg(palette.accent)),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(
                "Ctrl+O opens browser · Ctrl+Y copies link · Waiting for sign-in…",
            ));
            if *attempt.mode().get() == IntegrationOAuthMode::Code {
                lines.push(Line::from(format!(
                    "Authorization code: {}",
                    "•".repeat(pane.input.chars().count().min(48))
                )));
                lines.push(Line::from("Paste the code, then Enter"));
            }
        }
        Screen::Rename(_) => {
            lines.push(Line::from("Connection label:"));
            lines.push(Line::from(format!("› {}", literal(&pane.input))));
        }
        Screen::Remove(_) => {
            lines.push(Line::from("Disconnect this saved credential?"));
            lines.push(Line::from("Type disconnect and press Enter to confirm."));
            lines.push(Line::from(format!("› {}", literal(&pane.input))));
        }
    }
    let options_height = usize::from(inner.height.saturating_sub(5))
        .saturating_sub(lines.len())
        .max(1);
    pane.page = options_height;
    if pane.selected < pane.offset {
        pane.offset = pane.selected;
    }
    if pane.selected >= pane.offset + options_height {
        pane.offset = pane
            .selected
            .saturating_add(1)
            .saturating_sub(options_height);
    }
    for (index, option) in options
        .into_iter()
        .enumerate()
        .skip(pane.offset)
        .take(options_height)
    {
        lines.push(
            Line::from(format!(
                "{}{}",
                if index == pane.selected { "› " } else { "  " },
                grapt_client::cells::cut(&option, usize::from(inner.width.saturating_sub(2)))
            ))
            .style(if index == pane.selected {
                palette.normal().fg(palette.accent)
            } else {
                palette.normal()
            }),
        );
    }
    lines.push(Line::from(""));
    if !pane.notice.is_empty() {
        lines.push(Line::from(literal(&pane.notice)).style(palette.normal().fg(palette.accent)));
    }
    lines.push(Line::from(footer).style(palette.normal().fg(palette.muted)));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

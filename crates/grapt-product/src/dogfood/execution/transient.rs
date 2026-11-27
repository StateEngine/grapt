//! Bounded text generation through the ordinary provider plan and opener.
use super::*;

pub(in crate::dogfood) enum TransientContext {
    Plugin {
        history: Vec<grapt_provider::CodexInputItem>,
        hook: Option<serde_json::Value>,
    },
    SideQuestion,
}

impl LiveExecutor {
    /// Generate one bounded text result against a Session snapshot without
    /// admitting a new conversation message. Provider target issuance, route
    /// selection, opener capacity, transport deadlines and stream validation
    /// are the same owners used by ordinary coding attempts. Dropping the
    /// reader remains the cancellation primitive at the provider boundary.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(in crate::dogfood) async fn transient_generate(
        &self,
        session: SessionId,
        prompt: String,
        source: TransientContext,
    ) -> Result<String, String> {
        let side_question = matches!(&source, TransientContext::SideQuestion);
        if !side_question {
            if self.location.ephemeral {
                return Err("ephemeral sessions do not support plugin execution".to_owned());
            }
            self.refresh_published_tools(&session).await;
        }
        let (record, policy) = {
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            let record = SessionPorts::new(Arc::clone(&self.inbox), &authorities)
                .get(&session)
                .map_err(|error| error.to_string())?;
            let policy = super::super::permission_preset::effective(
                &self.location.service_root,
                &record,
                |id| {
                    SessionPorts::new(Arc::clone(&self.inbox), &authorities)
                        .get(id)
                        .map_err(|error| error.to_string())
                },
            )?;
            (record, policy)
        };
        let providers = self.location.provider_snapshot()?;
        let selected = record.model.clone().unwrap_or_else(|| {
            serde_json::json!({
                "providerID":providers.configured_provider.as_str(),
                "id":providers.configured_model.as_str(),
            })
        });
        let targets = providers.targets.selected(&selected)?;
        let first_target = targets.issue()?;
        let opener = providers
            .openers
            .get(first_target.provider())
            .ok_or_else(|| "selected provider opener absent".to_owned())?;
        let step =
            StepNumber::new(1).ok_or_else(|| "transient Step identity is invalid".to_owned())?;
        let cache = super::super::cache_scope::session_cache_key(
            &self.location.service_root,
            &self.location.inbox,
            &session,
            &record.project,
            &first_target,
        )?;
        let location = self
            .location
            .locations
            .resolve(&session, record.placement.directory())
            .await?;
        let (mut context, context_hook) = match source {
            TransientContext::Plugin { history, hook } => (history, hook),
            TransientContext::SideQuestion => {
                (self.side_context(&session, &first_target, &targets)?, None)
            }
        };
        let mut published = if side_question {
            super::super::mcp::PublishedTools::default()
        } else {
            (*self.published.get(&session)).clone()
        };
        if let Some(context_hook) = context_hook {
            apply_generation_context_patch(&context_hook, &mut context, &mut published)?;
        }
        let mut plan = ProductPlan::transient(
            targets.protocol_family(),
            prompt,
            cache,
            first_target,
            targets,
            Arc::new(published),
            context,
            step,
        )
        .with_codex_session(session.clone())
        .with_policy(policy);
        if side_question {
            plan = plan.text_only();
        } else {
            let roots = super::super::instructions::InstructionRoots::discover(
                &location,
                &self.plugins,
                record.agent.as_deref().unwrap_or("default"),
            )
            .await?;
            plan = plan
                .with_shell_description(
                    self.location
                        .shell_description(Path::new(record.placement.directory()))?,
                )
                .with_instructions(super::super::instructions::EffectiveInstructions {
                    roots,
                    inbox: Arc::clone(&self.inbox),
                    session: session.clone(),
                    ids: Arc::clone(&self.ids) as Arc<dyn IdAuthority + Send + Sync>,
                    clock: Arc::clone(&self.clock) as Arc<dyn ClockAuthority + Send + Sync>,
                });
        }
        let planned = plan
            .plan(step, 1, HistoryGeneration::new(0))
            .map_err(|error| error.detail)?;
        plan.context_admission(&planned)
            .map_err(|error| format!("transient generation admission failed: {error:?}"))?
            .map_or(Ok(()), |_| {
                Err("transient generation exceeds the selected model context".to_owned())
            })?;
        let attempt = PhysicalAttemptId::new(format!("pat_{}", self.ids.mint().as_str()))
            .ok_or_else(|| "transient provider attempt identity is invalid".to_owned())?;
        let opened = opener
            .open(attempt, planned.authority, planned.request)
            .map_err(|error| error.to_string())?;
        let (_custody, mut reader) = opened.into_parts();
        let mut validator = StreamValidator::new();
        let mut text = String::new();
        let mut terminal = false;
        let mut failure = None;
        while let Some(fragment) = reader.next_fragment().await {
            let accepted = validator
                .accept(&fragment)
                .map_err(|error| format!("transient provider output is invalid: {error}"))?;
            match fragment {
                AttemptFragment::TextDelta { text: delta, .. } => {
                    let output_bound = if side_question {
                        65_536
                    } else {
                        MAX_TRANSIENT_GENERATION_BYTES
                    };
                    if text.len().saturating_add(delta.len()) > output_bound {
                        return Err(format!(
                            "transient generation exceeds the {output_bound}-byte output bound"
                        ));
                    }
                    text.push_str(&delta);
                }
                AttemptFragment::ProviderFailure { class, message, .. } => {
                    failure = Some(format!("provider generation failed ({class:?}): {message}"));
                }
                AttemptFragment::ToolCalled { .. } if side_question => {
                    return Err("The side question requested a tool; no tool was run.".into());
                }
                _ => {}
            }
            terminal |= accepted.terminal;
        }
        self.report_dispatch_samples();
        if !terminal {
            return Err("transient provider stream ended without a terminal".to_owned());
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        if side_question && text.trim().is_empty() {
            return Err("The model returned no text for this side question.".into());
        }
        Ok(text)
    }
}

/// Refuses a `session.generate` context patch that [`apply_generation_context_patch`]
/// could not apply, so the callback chain can skip the callback that produced it.
pub(in crate::dogfood) fn check_generation_patch(patch: &serde_json::Value) -> Result<(), String> {
    system_parts(patch)?;
    for (name, value) in tool_map(patch)? {
        patched_tool(name, value)?;
    }
    Ok(())
}

fn apply_generation_context_patch(
    patch: &serde_json::Value,
    context: &mut Vec<grapt_provider::CodexInputItem>,
    published: &mut super::super::mcp::PublishedTools,
) -> Result<(), String> {
    let mut prefixed = system_parts(patch)?;
    prefixed.append(context);
    *context = prefixed;
    let tools = tool_map(patch)?;
    let offered = patch
        .get("offered")
        .and_then(serde_json::Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut updated = Vec::new();
    for mut definition in std::mem::take(&mut published.definitions) {
        let Some(value) = tools.get(&definition.name) else {
            // Only a tool the hook was offered can be removed by leaving it out.
            if !offered
                .iter()
                .any(|name| name.as_str() == Some(definition.name.as_str()))
            {
                updated.push(definition);
            }
            continue;
        };
        (definition.description, definition.input_schema) = patched_tool(&definition.name, value)?;
        updated.push(definition);
    }
    published.definitions = updated;
    Ok(())
}

/// The patch's system parts as system messages, within the patch bounds.
fn system_parts(patch: &serde_json::Value) -> Result<Vec<grapt_provider::CodexInputItem>, String> {
    const MAX_PATCH_BYTES: usize = 1024 * 1024;
    let bytes = serde_json::to_vec(patch).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_PATCH_BYTES {
        return Err(format!(
            "session.generate context hook exceeds the {MAX_PATCH_BYTES}-byte bound"
        ));
    }
    let system = patch
        .get("system")
        .and_then(serde_json::Value::as_array)
        .filter(|parts| parts.len() <= 128)
        .ok_or_else(|| {
            "session.generate context hook returned an invalid system array".to_owned()
        })?;
    system
        .iter()
        .map(|part| {
            let text = part
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    "session.generate context hook returned a non-text system part".to_owned()
                })?;
            Ok(grapt_provider::CodexInputItem::Message(
                grapt_provider::InputMessage {
                    role: grapt_provider::MessageRole::System,
                    content: text.to_owned(),
                },
            ))
        })
        .collect()
}

fn tool_map(
    patch: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, String> {
    patch
        .get("tools")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "session.generate context hook returned an invalid tool map".to_owned())
}

/// One patched tool's description and encoded input schema.
fn patched_tool(name: &str, value: &serde_json::Value) -> Result<(Option<String>, String), String> {
    let description = match value.get("description") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(description)) => Some(description.clone()),
        Some(_) => {
            return Err(format!(
                "session.generate context hook returned an invalid description for {name}"
            ))
        }
    };
    let schema = value
        .get("input")
        .filter(|schema| schema.is_object())
        .ok_or_else(|| {
            format!("session.generate context hook returned an invalid input schema for {name}")
        })?;
    let schema = serde_json::to_string(schema).map_err(|error| error.to_string())?;
    Ok((description, schema))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// The generation chain skips exactly the patches the transient plan could
    /// not apply, so a valid patch must pass and each malformed one must not.
    #[test]
    fn the_generation_patch_check_matches_what_the_plan_applies() {
        let tool = json!({"description": "echo", "input": {"type": "object"}});
        let valid = json!({"system": [{"text": "context"}], "tools": {"echo": tool}});
        assert_eq!(super::check_generation_patch(&valid), Ok(()));
        for malformed in [
            json!({"system": [{"text": 1}], "tools": {}}),
            json!({"system": "context", "tools": {}}),
            json!({"system": [], "tools": []}),
            json!({"system": [], "tools": {"echo": {"description": 1, "input": {}}}}),
            json!({"system": [], "tools": {"echo": {"input": "schema"}}}),
        ] {
            assert!(
                super::check_generation_patch(&malformed).is_err(),
                "{malformed}"
            );
        }
    }

    /// A plugin patch can only drop the tools its hook was offered: a session
    /// tool the hook never saw, such as a healthy MCP tool, stays published.
    #[test]
    fn a_plugin_patch_keeps_tools_its_hook_was_not_offered() -> Result<(), String> {
        let mut published = crate::dogfood::mcp::PublishedTools::default();
        for name in ["mcp_lookup", "plugin_echo", "plugin_gone"] {
            published.definitions.push(grapt_provider::ToolDefinition {
                input_format: grapt_provider::ToolInputFormat::Json,
                name: name.to_owned(),
                description: None,
                input_schema: "{}".to_owned(),
            });
        }
        let patch = json!({
            "system": [],
            "tools": {"plugin_echo": {"description": "patched", "input": {"type": "object"}}},
            "offered": ["plugin_echo", "plugin_gone"],
        });
        super::apply_generation_context_patch(&patch, &mut Vec::new(), &mut published)?;
        let names: Vec<_> = published
            .definitions
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, ["mcp_lookup", "plugin_echo"]);
        Ok(())
    }
}

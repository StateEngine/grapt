use super::*;

pub(super) struct ContextHook {
    pub(super) plugins: Arc<super::super::plugins::PluginRuntime>,
    pub(super) generation: Arc<grapt_plugin::PluginGeneration>,
    pub(super) session: grapt_types::SessionId,
    pub(super) agent: grapt_types::AgentId,
    pub(super) principal: grapt_api::SealedPrincipal,
}

const MAX_HOOK_ITEMS: usize = 512;

pub(super) fn apply_session_context_hook(
    hook: &ContextHook,
    model: &ResolvedModelTarget,
    input: &mut Vec<grapt_provider::CodexInputItem>,
    tools: &mut Vec<grapt_provider::ToolDefinition>,
    boundary: &mut RequestBoundary,
) -> Result<(), String> {
    const MAX_HOOK_CONTEXT_BYTES: usize = 4 * 1024 * 1024;

    let (system, messages, structural) = project_context_input(input);
    let original_messages = serde_json::Value::Array(messages.clone());
    let mut tool_map = serde_json::Map::new();
    for tool in tools.iter() {
        let schema: serde_json::Value = serde_json::from_str(&tool.input_schema)
            .map_err(|error| format!("tool {} has invalid input schema: {error}", tool.name))?;
        tool_map.insert(
            tool.name.clone(),
            serde_json::json!({"description":tool.description,"input":schema}),
        );
    }
    let value = serde_json::json!({
        "sessionID":hook.session.as_str(),
        "agent":hook.agent.as_str(),
        "model":{
            "providerID":model.provider().as_str(),
            "id":model.model().as_str(),
            "variant":model.model_ref().variant().map(grapt_types::ModelVariantId::as_str),
        },
        "system":system,
        "messages":messages,
        "tools":tool_map,
    });
    let authority = grapt_plugin::CallAuthority::Scoped {
        scope: Arc::new(super::super::plugins::PluginCallScope {
            principal: hook.principal.clone(),
            session: hook.session.clone(),
            agent: hook.agent.clone(),
            message: None,
            operations: super::super::plugins::SESSION_CALL_OPERATIONS,
        }),
        call: format!("session-context:{}", hook.session.as_str()),
    };
    let Some(value) = hook.plugins.apply_callbacks_blocking(
        &hook.generation,
        "session",
        "hook.context",
        value,
        authority,
    )?
    else {
        return Ok(());
    };
    let encoded = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    if encoded.len() > MAX_HOOK_CONTEXT_BYTES {
        return Err(format!(
            "session context hook exceeds the {MAX_HOOK_CONTEXT_BYTES}-byte bound"
        ));
    }
    let transformed = transformed_input(input, &value, structural, &original_messages)?;
    let transformed_tools = value
        .get("tools")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "session context hook returned an invalid tool map".to_owned())?;
    let mut updated_tools = Vec::new();
    for tool in tools.iter() {
        let Some(updated) = transformed_tools.get(&tool.name) else {
            continue;
        };
        let description = match updated.get("description") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(value)) => Some(value.clone()),
            Some(_) => {
                return Err(format!(
                    "session context hook returned an invalid description for {}",
                    tool.name
                ))
            }
        };
        // A callback may hand back a schema the provider cannot carry, including
        // none at all. That is the tool-eligibility owner's decision, not this
        // transport's: pass the value through and let the final judgement name
        // it per definition, so one bad definition is excluded rather than
        // ending the whole turn.
        let schema = updated.get("input").unwrap_or(&serde_json::Value::Null);
        updated_tools.push(grapt_provider::ToolDefinition {
            input_format: tool.input_format.clone(),
            name: tool.name.clone(),
            description,
            input_schema: serde_json::to_string(schema).map_err(|error| error.to_string())?,
        });
    }
    boundary.after_hook(
        system_prefix_len(input),
        system_prefix_len(&transformed),
        structural,
    );
    *input = transformed;
    *tools = updated_tools;
    Ok(())
}

fn system_prefix_len(input: &[grapt_provider::CodexInputItem]) -> usize {
    input
        .iter()
        .take_while(|item| {
            matches!(item,
        grapt_provider::CodexInputItem::Message(message)
            if message.role == grapt_provider::MessageRole::System)
        })
        .count()
}

fn project_context_input(
    input: &[grapt_provider::CodexInputItem],
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>, bool) {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    let mut structural = false;
    let prefix_len = system_prefix_len(input);
    for (index, item) in input.iter().enumerate() {
        match item {
            grapt_provider::CodexInputItem::Message(message)
                if index < prefix_len && message.role == grapt_provider::MessageRole::System =>
            {
                system.push(serde_json::json!({"type":"text","text":message.content}));
            }
            grapt_provider::CodexInputItem::Message(message) => {
                messages.push(serde_json::json!({
                    "role":match message.role {
                        grapt_provider::MessageRole::System => "system",
                        grapt_provider::MessageRole::User => "user",
                        grapt_provider::MessageRole::Assistant => "assistant",
                    },
                    "content":[{"type":"text","text":message.content}],
                }));
            }
            _ => structural = true,
        }
    }
    (system, messages, structural)
}

fn transformed_input(
    input: &[grapt_provider::CodexInputItem],
    value: &serde_json::Value,
    structural: bool,
    original_messages: &serde_json::Value,
) -> Result<Vec<grapt_provider::CodexInputItem>, String> {
    let transformed_system = value
        .get("system")
        .and_then(serde_json::Value::as_array)
        .filter(|rows| rows.len() <= MAX_HOOK_ITEMS)
        .ok_or_else(|| "session context hook returned an invalid system array".to_owned())?;
    let transformed_messages = value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .filter(|rows| rows.len() <= MAX_HOOK_ITEMS)
        .ok_or_else(|| "session context hook returned an invalid messages array".to_owned())?;
    if structural && value.get("messages") != Some(original_messages) {
        return Err(
            "session context hook cannot rewrite history containing typed tool or replay items"
                .to_owned(),
        );
    }
    let mut transformed = Vec::new();
    for part in transformed_system {
        let text = part
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "session context hook returned a non-text system part".to_owned())?;
        transformed.push(grapt_provider::CodexInputItem::Message(
            grapt_provider::InputMessage {
                role: grapt_provider::MessageRole::System,
                content: text.to_owned(),
            },
        ));
    }
    if structural {
        // Instruction epochs are System messages in the history tail. A hook
        // may transform the initial prefix without moving or deleting those epochs.
        transformed.extend(input.iter().skip(system_prefix_len(input)).cloned());
    } else {
        for message in transformed_messages {
            let role = match message.get("role").and_then(serde_json::Value::as_str) {
                Some("system") => grapt_provider::MessageRole::System,
                Some("user") => grapt_provider::MessageRole::User,
                Some("assistant") => grapt_provider::MessageRole::Assistant,
                _ => return Err("session context hook returned an invalid message role".to_owned()),
            };
            let content = message
                .get("content")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    "session context hook returned invalid message content".to_owned()
                })?;
            for part in content {
                let text = part
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        "session context hook returned a non-text message part".to_owned()
                    })?;
                transformed.push(grapt_provider::CodexInputItem::Message(
                    grapt_provider::InputMessage {
                        role,
                        content: text.to_owned(),
                    },
                ));
            }
        }
    }
    Ok(transformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_provider::{CodexInputItem, InputMessage, MessageRole, ReasoningEffort};
    use serde_json::json;

    #[test]
    fn an_unchanged_hook_preserves_instruction_epochs_and_generation_controls() -> Result<(), String>
    {
        let message = |role, text: &str| {
            CodexInputItem::Message(InputMessage {
                role,
                content: text.to_owned(),
            })
        };
        let input = vec![
            message(MessageRole::System, "initial instructions"),
            message(MessageRole::User, "first request"),
            CodexInputItem::AssistantResponseStart,
            message(MessageRole::Assistant, "first answer"),
            message(MessageRole::System, "updated instructions"),
            CodexInputItem::EffortUpdate(ReasoningEffort::High),
            message(MessageRole::User, "second request"),
        ];
        let (system, messages, structural) = project_context_input(&input);
        assert_eq!(
            system,
            vec![json!({"type":"text","text":"initial instructions"})]
        );
        assert!(messages.iter().any(|message| message["role"] == "system"));
        let original_messages = serde_json::Value::Array(messages.clone());
        let mut value = json!({"system":system,"messages":messages});
        assert_eq!(
            transformed_input(&input, &value, structural, &original_messages)?,
            input
        );
        value["messages"] = json!([]);
        assert!(transformed_input(&input, &value, structural, &original_messages).is_err());
        Ok(())
    }
}

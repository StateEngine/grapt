//! History-free plugin generation through the compiled product provider path.

use std::sync::Arc;

use grapt_plugin::{CallAuthority, PluginHostContext};
use grapt_provider::{CodexInputItem, InputMessage, MessageRole};
use serde_json::{json, Value};

use super::authority::Authorized;
use super::ProductPluginHost;

const MAX_PROMPT_BYTES: usize = 1024 * 1024;
const MAX_CONTEXT_ITEMS: usize = 512;
const MAX_CONTEXT_BYTES: usize = 4 * 1024 * 1024;

impl ProductPluginHost {
    pub(crate) fn session_generate(
        &self,
        context: &PluginHostContext,
        application: &Arc<super::super::ServerApplication>,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let call = authorized.call()?;
        let session = call.scope.session.clone();
        let prompt = payload
            .get("prompt")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= MAX_PROMPT_BYTES)
            .ok_or_else(|| {
                format!(
                    "session.generate requires a nonempty prompt within {MAX_PROMPT_BYTES} bytes"
                )
            })?
            .to_owned();
        let context_hook =
            self.generation_context_hooks(context, application, authorized, &session, &prompt)?;
        let history = generation_context(application, &session)?;
        let application = Arc::clone(application);
        self.handle.block_on(async move {
            let text = application
                .executor
                .0
                .transient_generate(
                    session,
                    prompt,
                    super::super::execution::TransientContext::Plugin {
                        history,
                        hook: context_hook,
                    },
                )
                .await?;
            Ok(Value::String(text))
        })
    }

    /// Run every plugin's `session.hook.context` over the transient request,
    /// each in its owning process under the calling grant.
    fn generation_context_hooks(
        &self,
        context: &PluginHostContext,
        application: &super::super::ServerApplication,
        authorized: &Authorized<'_>,
        session: &grapt_types::SessionId,
        prompt: &str,
    ) -> Result<Option<Value>, String> {
        let call = authorized.call()?;
        let Some(generation) = application
            .plugins
            .generation()
            .filter(|generation| generation.id() == context.generation())
        else {
            return Err("plugin generation is no longer active".to_owned());
        };
        let tools = generation
            .registrations()
            .iter()
            .flat_map(|registration| registration.tools.iter())
            .map(|tool| {
                (
                    tool.name.clone(),
                    json!({"description": tool.description, "input": tool.input_schema}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        // The plan may drop only tools the hook was offered; a session tool
        // it never saw, such as an MCP tool, is kept.
        let offered = tools.keys().cloned().collect::<Vec<_>>();
        let value = json!({
            "sessionID": session.as_str(),
            "system": [],
            "messages": [{"role": "user", "content": [{"type": "text", "text": prompt}]}],
            "tools": tools,
        });
        // A callback whose patch cannot be applied is skipped like one that
        // failed: the chain keeps the context from before it.
        let patch = |hooked: &Value| json!({"system": hooked["system"], "tools": hooked["tools"], "offered": offered});
        Ok(application
            .plugins
            .apply_checked_callbacks_blocking(
                (&generation, "session", "hook.context"),
                value,
                CallAuthority::Inherit {
                    parent: call.grant.clone(),
                    call: format!("session-generate:{}", call.grant.id()),
                },
                (super::OnCallbackFailure::Skip, &mut Vec::new()),
                &|hooked| super::super::execution::check_generation_patch(&patch(hooked)),
            )?
            .map(|hooked| patch(&hooked)))
    }
}

fn generation_context(
    application: &super::super::ServerApplication,
    session: &grapt_types::SessionId,
) -> Result<Vec<CodexInputItem>, String> {
    let mut context = Vec::new();
    let mut bytes = 0usize;
    application.visit_history_rows(session, |raw| {
        let row: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        let item = match row.get("type").and_then(Value::as_str) {
            Some("user" | "synthetic") => {
                row.get("text")
                    .and_then(Value::as_str)
                    .map(|text| InputMessage {
                        role: MessageRole::User,
                        content: text.to_owned(),
                    })
            }
            Some("system") => row
                .get("text")
                .and_then(Value::as_str)
                .map(|text| InputMessage {
                    role: MessageRole::System,
                    content: text.to_owned(),
                }),
            Some("assistant") => {
                let text = row
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<String>();
                (!text.is_empty()).then_some(InputMessage {
                    role: MessageRole::Assistant,
                    content: text,
                })
            }
            _ => None,
        };
        let Some(item) = item else {
            return Ok(());
        };
        bytes = bytes
            .checked_add(item.content.len())
            .ok_or_else(|| "session.generate context byte count overflowed".to_owned())?;
        if context.len() >= MAX_CONTEXT_ITEMS || bytes > MAX_CONTEXT_BYTES {
            return Err("session.generate context exceeds the bounded preview".to_owned());
        }
        context.push(CodexInputItem::Message(item));
        Ok(())
    })?;
    Ok(context)
}

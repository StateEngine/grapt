//! Plugin command evaluation through the product's command and tool owners.

use std::sync::Arc;

use grapt_api::u3a::ApplicationPort as _;
use grapt_types::IdAuthority as _;
use serde_json::{json, Value};

use super::authority::Authorized;
use super::ProductPluginHost;

const MAX_ARGUMENT_BYTES: usize = 64 * 1_024;
const MAX_SHELL_EXPANSIONS: usize = 16;

impl ProductPluginHost {
    pub(crate) fn session_command(
        &self,
        application: &Arc<super::super::ServerApplication>,
        authorized: &Authorized<'_>,
        payload: &Value,
    ) -> Result<Value, String> {
        let scope = authorized.call()?.scope;
        let session = scope.session.clone();
        let principal = scope.principal.clone();
        let name = payload
            .get("command")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .ok_or_else(|| "session.command requires a bounded command".to_owned())?;
        let arguments = payload
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if arguments.len() > MAX_ARGUMENT_BYTES {
            return Err(format!(
                "session.command arguments exceed the {MAX_ARGUMENT_BYTES}-byte bound"
            ));
        }
        let definition = application
            .plugins
            .command_registry()?
            .commands
            .get(name)
            .cloned()
            .ok_or_else(|| format!("Command not found: {name}"))?;
        let input = match payload.get("id").and_then(Value::as_str) {
            Some(raw) => raw
                .parse::<grapt_types::MessageId>()
                .map_err(|_| "session.command id is invalid".to_owned())?,
            None => format!("msg_{}", application.ids.mint().as_str())
                .parse()
                .map_err(|_| "minted command input id is invalid".to_owned())?,
        };
        let evaluated = evaluate_arguments(&definition.template, arguments);
        let application = Arc::clone(application);
        let payload = payload.clone();
        let name = name.to_owned();
        let wake = application.wake_slots.reserve(&session)?;
        self.handle.block_on(wake.run_activity(async move {
            let text = evaluate_shell(
                &application,
                session.clone(),
                input.clone(),
                principal.clone(),
                evaluated,
            )
            .await
            .map_err(|error| format!("Command {name} evaluation failed: {error}"))?;
            let selected_agent = definition.agent.or_else(|| {
                payload
                    .get("agent")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            if let Some(agent) = selected_agent {
                let request = super::super::decode(json!({"agent":agent}))?;
                super::super::selection::agent(&application, session.clone(), request)
                    .map_err(|error| format!("{error:?}"))?;
            }
            let selected_model = definition.model.or_else(|| payload.get("model").cloned());
            if let Some(model) = selected_model {
                let request = super::super::decode(json!({"model":model}))?;
                super::super::selection::model(&application, session.clone(), request)
                    .map_err(|error| format!("{error:?}"))?;
            }
            let mut request = json!({
                "id":input.as_str(),
                "text":text,
                "delivery":payload.get("delivery").cloned().unwrap_or_else(|| json!("steer")),
                "resume":payload.get("resume").cloned().unwrap_or(json!(true)),
            });
            for field in ["files", "agents"] {
                if let Some(value) = payload.get(field) {
                    request[field] = value.clone();
                }
            }
            let request = super::super::decode::<grapt_api::u3a::PromptRequestDto>(request)?;
            let wire =
                grapt_api::u3a::WireId::parse(session.as_str().to_owned()).ok_or_else(|| {
                    "session.command sessionID cannot be represented on the wire".to_owned()
                })?;
            let response = application
                .session_prompt(principal, wire, request)
                .await
                .map_err(|error| format!("{error:?}"))?;
            Self::encode(response)
        }))
    }
}

fn parse_arguments(input: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for ch in input.chars() {
        match (quote, ch) {
            (Some(held), value) if value == held => quote = None,
            (Some(_), value) => current.push(value),
            (None, '\'' | '"') => quote = Some(ch),
            (None, value) if value.is_whitespace() => {
                if !current.is_empty() {
                    values.push(std::mem::take(&mut current));
                }
            }
            (None, value) => current.push(value),
        }
    }
    if !current.is_empty() {
        values.push(current);
    }
    values
}

fn evaluate_arguments(template: &str, input: &str) -> String {
    let arguments = parse_arguments(input);
    let mut placeholders = Vec::new();
    let bytes = template.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'$' {
            let mut end = index + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > index + 1 {
                if let Ok(position) = template[index + 1..end].parse::<usize>() {
                    placeholders.push(position);
                }
                index = end;
                continue;
            }
        }
        index += 1;
    }
    let last = placeholders.iter().copied().max().unwrap_or(0);
    let mut output = String::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'$' {
            let mut end = index + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > index + 1 {
                let position = template[index + 1..end].parse::<usize>().unwrap_or(0);
                let argument_index = position.saturating_sub(1);
                if argument_index < arguments.len() {
                    if position == last {
                        output.push_str(&arguments[argument_index..].join(" "));
                    } else {
                        output.push_str(&arguments[argument_index]);
                    }
                }
                index = end;
                continue;
            }
        }
        let Some(ch) = template[index..].chars().next() else {
            break;
        };
        output.push(ch);
        index += ch.len_utf8();
    }
    let had_all = template.contains("$ARGUMENTS");
    output = output.replace("$ARGUMENTS", input);
    if placeholders.is_empty() && !had_all && !input.trim().is_empty() {
        output.push_str("\n\n");
        output.push_str(input);
    }
    output.trim().to_owned()
}

async fn evaluate_shell(
    application: &super::super::ServerApplication,
    session: grapt_types::SessionId,
    message: grapt_types::MessageId,
    principal: grapt_api::SealedPrincipal,
    input: String,
) -> Result<String, String> {
    let mut output = String::new();
    let mut rest = input.as_str();
    let mut count = 0usize;
    while let Some(start) = rest.find("!`") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('`')
            .ok_or_else(|| "shell interpolation has no closing backtick".to_owned())?;
        count += 1;
        if count > MAX_SHELL_EXPANSIONS {
            return Err(format!(
                "command template exceeds the {MAX_SHELL_EXPANSIONS}-shell interpolation bound"
            ));
        }
        let command = &after[..end];
        if command.trim().is_empty() {
            return Err("shell interpolation command is empty".to_owned());
        }
        let record = application.get_record(&session)?;
        let agent = grapt_types::AgentId::new(record.agent.unwrap_or_else(|| "default".to_owned()))
            .ok_or_else(|| "session agent identity is invalid".to_owned())?;
        let value = application
            .executor
            .0
            .plugin_shell_interpolation(
                session.clone(),
                message.clone(),
                agent,
                principal.clone(),
                command.to_owned(),
            )
            .await?;
        output.push_str(&value);
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_preserve_last_placeholder_and_fallback() {
        assert_eq!(
            evaluate_arguments("review $1 then $2", "one 'two words' three"),
            "review one then two words three"
        );
        assert_eq!(evaluate_arguments("review", "branch"), "review\n\nbranch");
        assert_eq!(
            evaluate_arguments("all $ARGUMENTS", "one two"),
            "all one two"
        );
    }
}

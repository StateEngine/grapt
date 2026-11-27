//! MCP argument groups use the existing quoted command parser, never expansion.
use super::{Completion, View};
use grapt_api::u3a::McpContentRequestDto;
use grapt_client::{client::McpContent, Client, Connection};

pub(super) const STAGED_IMAGES: &str = "MCP content commands cannot include staged images. Send or remove the images first; draft kept.";

pub(super) fn content_command<C: Connection>(
    client: &mut Client<C>,
    command: &str,
    done: &mut Completion,
    defer_prompt: bool,
) -> bool {
    let Some(args) = grapt_client::composer_commands::argument(command, "/mcp") else {
        return false;
    };
    let action = args.split_whitespace().next().unwrap_or_default();
    if !matches!(action, "resources" | "read" | "pin" | "unpin") {
        return false;
    }
    if action == "read" && defer_prompt {
        done.notice = STAGED_IMAGES.into();
        return true;
    }
    let request = (|| -> Result<_, String> {
        let words = crate::slash::expand::split(args)?;
        match words.as_slice() {
            [action, server] if action == "resources" => Ok((server.clone(), McpContentRequestDto::Resources {})),
            [action, server, uri] if action == "pin" || action == "unpin" => Ok((server.clone(), serde_json::from_value(serde_json::json!({"action":"pin_resource", "uri":uri,"pinned":action == "pin"})).map_err(|error| error.to_string())?)),
            [action, server, uri] if action == "read" => Ok((server.clone(), serde_json::from_value(serde_json::json!({"action":"read_resource", "uri":uri})).map_err(|error| error.to_string())?)),
            _ => Err("Use /mcp resources <server> or /mcp read/pin/unpin <server> <uri>; quote names or URIs containing spaces.".into()),
        }
    })();
    match request {
        Ok((server, request)) => begin(client, &server, request, done),
        Err(error) => done.notice = error,
    }
    true
}

pub(super) fn prompt<C: Connection>(
    client: &mut Client<C>,
    binding: &grapt_client::wire::McpPrompt,
    args: &str,
    done: &mut Completion,
) {
    let request = arguments(binding, args).and_then(|arguments| {
        serde_json::from_value(
            serde_json::json!({"action":"prompt","name":binding.name,"arguments":arguments}),
        )
        .map_err(|error| error.to_string())
    });
    match request {
        Ok(request) => begin(client, &binding.server, request, done),
        Err(error) => done.notice = error,
    }
}

fn arguments(
    binding: &grapt_client::wire::McpPrompt,
    args: &str,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let words = crate::slash::expand::split(args)?;
    let named = words.iter().any(|word| {
        word.split_once('=')
            .is_some_and(|(name, _)| binding.arguments.iter().any(|arg| arg.name == name))
    });
    let mut values = std::collections::BTreeMap::new();
    if named {
        for word in words {
            let (name, value) = word
                .split_once('=')
                .ok_or("Use name=value for every named prompt argument; draft kept.")?;
            if !binding.arguments.iter().any(|arg| arg.name == name) {
                return Err(format!("Unknown prompt argument {name}; draft kept."));
            }
            if values.insert(name.to_owned(), value.to_owned()).is_some() {
                return Err(format!("Duplicate prompt argument {name}; draft kept."));
            }
        }
    } else {
        if words.len() > binding.arguments.len() {
            return Err("Too many prompt arguments; quote multiword values. Draft kept.".into());
        }
        values.extend(
            binding
                .arguments
                .iter()
                .zip(words)
                .map(|(arg, value)| (arg.name.clone(), value)),
        );
    }
    for arg in &binding.arguments {
        if arg.required && values.get(&arg.name).is_none_or(|value| value.is_empty()) {
            return Err(format!("Prompt needs argument <{}>; draft kept.", arg.name));
        }
    }
    Ok(values)
}

fn begin<C: Connection>(
    client: &mut Client<C>,
    server: &str,
    request: McpContentRequestDto,
    done: &mut Completion,
) {
    match client.begin_mcp_content(server, request) {
        Ok(Some(content)) => complete(client, content, done),
        Ok(None) => {
            done.session = client.selected().cloned();
            done.notice = "Reading MCP content; draft kept.".into();
        }
        Err(error) => done.notice = error.to_string(),
    }
}

pub(in crate::backend) fn complete<C: Connection>(
    client: &mut Client<C>,
    content: McpContent,
    done: &mut Completion,
) {
    done.session = client.selected().cloned();
    match content {
        McpContent::Subscription(notice) => {
            done.consume = true;
            done.notice = notice;
            done.view = Some(View::Mcp {
                rows: grapt_client::view::mcp_summary_lines(client, 120),
                verbose: false,
            });
        }
        McpContent::Resources(lines) => {
            done.notice = "MCP resources ready. Use /mcp read <server> <uri> to add one.".into();
            done.consume = true;
            done.view = Some(View::Text(lines));
        }
        McpContent::User(content) => {
            if let Err(error) = client.create_draft_session() {
                done.notice = error;
                return;
            }
            done.session = client.selected().cloned();
            done.prepared_prompt = Some(content.text.as_str().into());
            done.prepared_files = content.files.as_slice().to_vec();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declared_arguments_keep_quoted_code_and_refuse_ambiguous_named_values() -> Result<(), String>
    {
        let binding = grapt_client::wire::McpPrompt {
            server: "book".into(),
            name: "review".into(),
            arguments: vec![
                grapt_client::wire::McpArgument {
                    name: "subject".into(),
                    required: true,
                },
                grapt_client::wire::McpArgument {
                    name: "style".into(),
                    required: false,
                },
            ],
        };
        assert_eq!(
            arguments(&binding, "'code x=1'")?
                .get("subject")
                .map(String::as_str),
            Some("code x=1")
        );
        assert_eq!(
            arguments(&binding, "subject='code x=1' style=terse")?
                .get("style")
                .map(String::as_str),
            Some("terse")
        );
        for invalid in [
            "",
            "one two three",
            "subject=one subject=two",
            "subject=one unknown=two",
            "one style=terse",
        ] {
            assert!(
                arguments(&binding, invalid).is_err(),
                "accepted invalid declared arguments"
            );
        }
        Ok(())
    }
}

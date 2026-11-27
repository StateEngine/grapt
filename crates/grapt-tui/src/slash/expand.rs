// Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// Modified for bounded, single-pass command prompts, one-based positional arguments and quoted argument grouping.
mod claude;

const MAX_BYTES: usize = 64 * 1024;

#[cfg(test)]
pub(crate) fn expand(template: &str, arguments: &str) -> Result<String, String> {
    expand_with_paths(template, arguments, None)
}
pub(crate) fn expand_with_paths(
    template: &str,
    arguments: &str,
    paths: Option<&grapt_client::wire::ClaudePluginPaths>,
) -> Result<String, String> {
    if let Some(paths) = paths {
        return claude::expand(template, arguments, paths);
    }
    if template.len() > MAX_BYTES || arguments.len() > MAX_BYTES {
        return Err("Command template or arguments exceed 64 KiB; draft kept".into());
    }
    let mut output = String::new();
    let mut rest = template;
    // Existing product commands assign remaining groups to the last position.
    let last = template
        .split('$')
        .skip(1)
        .filter_map(|part| {
            let digit = part.as_bytes().first().copied()?;
            ((b'1'..=b'9').contains(&digit)
                && !part.as_bytes().get(1).is_some_and(u8::is_ascii_digit))
            .then_some(digit)
        })
        .max();
    let mut consumed = false;
    let mut words = None;
    while let Some(offset) = rest.find('$') {
        append(&mut output, &rest[..offset])?;
        rest = &rest[offset..];
        if let Some(after) = rest.strip_prefix("$ARGUMENTS") {
            append(&mut output, arguments)?;
            consumed = true;
            rest = after;
        } else if let Some(digit) = rest
            .as_bytes()
            .get(1)
            .copied()
            .filter(|b| (b'1'..=b'9').contains(b))
        {
            if rest.as_bytes().get(2).is_some_and(u8::is_ascii_digit) {
                append(&mut output, "$")?;
                rest = &rest[1..];
                continue;
            }
            if words.is_none() {
                words = Some(split(arguments)?);
            }
            let index = usize::from(digit - b'1');
            let value = words
                .as_ref()
                .and_then(|values| values.get(index))
                .ok_or_else(|| {
                    format!("Command needs argument ${}; draft kept", char::from(digit))
                })?;
            if last == Some(digit) {
                let remainder = words
                    .as_ref()
                    .map(|values| values[index..].join(" "))
                    .unwrap_or_default();
                append(&mut output, &remainder)?;
            } else {
                append(&mut output, value)?;
            }
            consumed = true;
            rest = &rest[2..];
        } else {
            append(&mut output, "$")?;
            rest = &rest[1..];
        }
    }
    append(&mut output, rest)?;
    if !consumed && !arguments.is_empty() {
        append(&mut output, "\n\nArguments: ")?;
        append(&mut output, arguments)?;
    }
    if output.trim().is_empty() {
        return Err("Command template is empty; draft kept".into());
    }
    Ok(output)
}

fn append(output: &mut String, part: &str) -> Result<(), String> {
    if output.len().saturating_add(part.len()) > MAX_BYTES {
        return Err("Expanded command exceeds 64 KiB; draft kept".into());
    }
    output.push_str(part);
    Ok(())
}

pub(crate) fn split(arguments: &str) -> Result<Vec<String>, String> {
    let mut values = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut active = false;
    let mut characters = arguments.chars().peekable();
    while let Some(ch) = characters.next() {
        if ch == '\\'
            && quote != Some('\'')
            && characters.peek().is_some_and(|next| {
                *next == '"' || *next == '\'' || quote.is_none() && next.is_whitespace()
            })
        {
            if let Some(next) = characters.next() {
                word.push(next);
            }
            active = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
            active = true;
        } else if quote.is_none() && ch.is_whitespace() {
            if active {
                values.push(std::mem::take(&mut word));
                active = false;
                if values.len() > 64 {
                    return Err("Command exceeds 64 arguments; draft kept".into());
                }
            }
        } else {
            word.push(ch);
            active = true;
        }
    }
    if quote.is_some() {
        return Err("Unclosed quoted command argument; draft kept".into());
    }
    if active {
        values.push(word);
    }
    if values.len() > 64 {
        return Err("Command exceeds 64 arguments; draft kept".into());
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_expand_once_and_quoted_paths_keep_their_boundaries() -> Result<(), String> {
        assert_eq!(expand("Review $ARGUMENTS", "a $1")?, "Review a $1");
        assert_eq!(
            expand("Read $1 then $2", r#""C:\src\a b.rs" '界 path'"#)?,
            "Read C:\\src\\a b.rs then 界 path"
        );
        assert_eq!(
            expand("Task with $100 budget", "branch")?,
            "Task with $100 budget\n\nArguments: branch"
        );
        assert_eq!(expand("Review $ARGUMENTS", "")?, "Review ");
        assert_eq!(
            expand("Read $1 then $2", "first second third")?,
            "Read first then second third"
        );
        assert_eq!(expand("9 issues $1", "a b")?, "9 issues a b");
        assert!(expand("Need $2", "one").is_err());
        assert!(expand("Need $1", "\"unclosed").is_err());
        Ok(())
    }
    #[test]
    fn repeated_placeholders_cannot_grow_past_the_prompt_bound() {
        assert!(expand(&"$ARGUMENTS".repeat(1000), &"界".repeat(1024)).is_err());
        assert!(expand("", "").is_err());
    }
}

#[cfg(test)]
#[test]
fn plugin_paths_remain_literal_while_command_arguments_expand() -> Result<(), String> {
    let paths = grapt_client::wire::ClaudePluginPaths {
        root: "/plugin $1 ${CLAUDE_PROJECT_DIR}".into(),
        data: "/data with spaces".into(),
        project: "/project $ARGUMENTS".into(),
    };
    assert_eq!(
        expand_with_paths(
            "${CLAUDE_PLUGIN_ROOT} ${CLAUDE_PLUGIN_DATA} ${CLAUDE_PROJECT_DIR} $ARGUMENTS",
            "chosen $1",
            Some(&paths)
        )?,
        "/plugin $1 ${CLAUDE_PROJECT_DIR} /data with spaces /project $ARGUMENTS chosen $1"
    );
    Ok(())
}

// Copyright 2023-2026 SpaceXAI. Apache-2.0 (licenses/xai-grok-build/).
// Quoted argument splitting adapted from Grapt's native slash expander.
// Modified for shared, bounded Claude argument and literal-path semantics.
//! Claude uses zero-based indexed arguments; native commands keep their grammar.
const MAX_BYTES: usize = 64 * 1024;
/// Paths supplied by the product owner; replacement bytes are never rescanned.
#[derive(Debug)]
pub struct Paths<'a> {
    pub root: &'a str,
    pub data: &'a str,
    pub project: &'a str,
}
fn append(output: &mut String, value: &str) -> Result<(), String> {
    if output.len().saturating_add(value.len()) > MAX_BYTES {
        return Err("expanded plugin prompt exceeds 64 KiB".into());
    }
    output.push_str(value);
    Ok(())
}

pub fn expand(template: &str, arguments: &str, paths: &Paths<'_>) -> Result<String, String> {
    if template.len() > MAX_BYTES || arguments.len() > MAX_BYTES {
        return Err("Plugin command or arguments exceed 64 KiB; draft kept".into());
    }
    let mut rest = template;
    let mut output = String::new();
    let mut words = None;
    let mut consumed = false;
    while let Some(offset) = rest.find('$') {
        let prefix = &rest[..offset];
        rest = &rest[offset..];
        if let Some((length, index)) = argument(rest) {
            let escaped = prefix.ends_with('\\')
                && !prefix.strip_suffix('\\').is_some_and(|p| p.ends_with('\\'));
            append(
                &mut output,
                if escaped {
                    &prefix[..prefix.len() - 1]
                } else {
                    prefix
                },
            )?;
            if escaped {
                append(&mut output, &rest[..length])?;
            } else if let Some(index) = index {
                if words.is_none() {
                    words = Some(split(arguments)?);
                }
                if let Some(value) = words.as_ref().and_then(|values| values.get(index)) {
                    append(&mut output, &expand_paths(value, paths)?)?;
                    consumed = true;
                } else {
                    append(&mut output, &rest[..length])?;
                }
            } else {
                append(&mut output, &expand_paths(arguments, paths)?)?;
                consumed = true;
            }
            rest = &rest[length..];
        } else {
            append(&mut output, prefix)?;
            if let Some((name, value)) = path(rest, paths) {
                append(&mut output, value)?;
                rest = &rest[name.len()..];
            } else {
                append(&mut output, "$")?;
                rest = &rest[1..];
            }
        }
    }
    append(&mut output, rest)?;
    if !consumed && !arguments.is_empty() {
        append(&mut output, "\n\nARGUMENTS: ")?;
        append(&mut output, &expand_paths(arguments, paths)?)?;
    }
    if output.trim().is_empty() {
        return Err("Plugin command is empty; draft kept".into());
    }
    Ok(output)
}
fn path<'a>(rest: &str, paths: &'a Paths<'_>) -> Option<(&'static str, &'a str)> {
    [
        ("${CLAUDE_PLUGIN_ROOT}", paths.root),
        ("${CLAUDE_PLUGIN_DATA}", paths.data),
        ("${CLAUDE_PROJECT_DIR}", paths.project),
    ]
    .into_iter()
    .find(|(name, _)| rest.starts_with(name))
}
fn expand_paths(text: &str, paths: &Paths<'_>) -> Result<String, String> {
    let mut rest = text;
    let mut output = String::new();
    while let Some(offset) = rest.find('$') {
        append(&mut output, &rest[..offset])?;
        rest = &rest[offset..];
        if let Some((name, value)) = path(rest, paths) {
            append(&mut output, value)?;
            rest = &rest[name.len()..];
        } else {
            append(&mut output, "$")?;
            rest = &rest[1..];
        }
    }
    append(&mut output, rest)?;
    Ok(output)
}
fn argument(rest: &str) -> Option<(usize, Option<usize>)> {
    if let Some(after) = rest.strip_prefix("$ARGUMENTS[") {
        if let Some(end) = after.find(']') {
            let digits = &after[..end];
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                return Some((12 + end, Some(digits.parse().ok()?)));
            }
        }
    }
    if rest.starts_with("$ARGUMENTS") {
        return Some((10, None));
    }
    let digits = rest
        .strip_prefix('$')?
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if digits == 0 {
        return None;
    }
    Some((1 + digits, Some(rest[1..1 + digits].parse().ok()?)))
}

#[cfg(test)]
#[test]
fn zero_based_arguments_missing_positions_and_escapes_keep_claude_semantics() -> Result<(), String>
{
    let paths = Paths {
        root: "/plugins $0 ${CLAUDE_PROJECT_DIR}",
        data: "/data",
        project: "/repo",
    };
    assert_eq!(
        expand(
            "$0 $ARGUMENTS[1] $2 $ARGUMENTS",
            "\"hello world\" second",
            &paths
        )?,
        "hello world second $2 \"hello world\" second"
    );
    assert_eq!(
        expand(
            r"\$0 $0 $1",
            "'$ARGUMENTS from yesterday' '${CLAUDE_PLUGIN_ROOT}'",
            &paths
        )?,
        "$0 $ARGUMENTS from yesterday /plugins $0 ${CLAUDE_PROJECT_DIR}"
    );
    assert_eq!(
        expand("No placeholder", "value", &paths)?,
        "No placeholder\n\nARGUMENTS: value"
    );
    Ok(())
}

fn split(arguments: &str) -> Result<Vec<String>, String> {
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

//! The same bounded YAML grammar used by the native Markdown catalog.
use yaml_rust2::{
    parser::{Event, Parser},
    Yaml, YamlLoader,
};

fn mapping(text: &str) -> Result<Option<Yaml>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().unwrap_or("");
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return Ok(None);
    }
    let start = first.len();
    let mut end = start;
    let mut boundary = None;
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            boundary = Some(end);
            break;
        }
        end += line.len();
        if end - start > 32 * 1024 {
            return Err("rule frontmatter exceeds 32 KiB".into());
        }
    }
    let raw = &text[start..boundary.ok_or("rule frontmatter has no closing delimiter")?];
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let mut parser = Parser::new_from_str(raw);
    let mut depth = 0usize;
    let mut ended = false;
    for _ in 0..2048 {
        match parser
            .next_token()
            .map_err(|_| "rule frontmatter is malformed")?
            .0
        {
            Event::StreamEnd => {
                ended = true;
                break;
            }
            Event::Alias(_) => return Err("rule frontmatter aliases are not supported".into()),
            Event::MappingStart(_, _) | Event::SequenceStart(_, _) => {
                depth += 1;
                if depth > 16 {
                    return Err("rule frontmatter exceeds depth 16".into());
                }
            }
            Event::MappingEnd | Event::SequenceEnd => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("invalid rule frontmatter depth")?;
            }
            _ => {}
        }
    }
    if !ended {
        return Err("rule frontmatter exceeds 2048 parser events".into());
    }
    let mut documents = YamlLoader::load_from_str(raw)
        .map_err(|_| "rule frontmatter is malformed or repeats a key")?;
    if documents.len() != 1 {
        return Err("rule frontmatter must contain one mapping".into());
    }
    let metadata = documents
        .pop()
        .ok_or("rule frontmatter mapping is absent")?;
    if metadata.as_hash().is_none() {
        return Err("rule frontmatter must be a mapping".into());
    }
    Ok(Some(metadata))
}

pub(super) fn agent_name(text: &str) -> Result<String, String> {
    let metadata = mapping(text)?.ok_or("Claude agent frontmatter is missing")?;
    let name = metadata["name"]
        .as_str()
        .ok_or("Claude agent name is missing")?;
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("Claude agent name must be nonempty, bounded and contain no controls".into());
    }
    Ok(name.to_owned())
}

pub(super) fn scoped(text: &str) -> Result<bool, String> {
    let Some(metadata) = mapping(text)? else {
        return Ok(false);
    };
    let patterns = match (&metadata["paths"], &metadata["globs"]) {
        (Yaml::BadValue, Yaml::BadValue) => return Ok(false),
        (patterns, Yaml::BadValue) | (Yaml::BadValue, patterns) => patterns,
        _ => return Err("rule cannot declare both paths and globs".into()),
    };
    let patterns = match patterns {
        Yaml::String(pattern) => vec![pattern.as_str()],
        Yaml::Array(patterns) => patterns
            .iter()
            .map(|pattern| pattern.as_str().ok_or("rule patterns must be strings"))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("rule paths must be a string or nonempty string list".into()),
    };
    if patterns.is_empty()
        || patterns.len() > 64
        || patterns.iter().any(|pattern| {
            pattern.is_empty()
                || pattern.len() > 1024
                || pattern.starts_with(['/', '!', '~'])
                || pattern.contains(['\\', ':', '\0'])
                || pattern.split('/').any(|part| part == "..")
        })
    {
        return Err(
            "rule patterns must be 1..64 relative forward-slash patterns, each at most 1024 bytes"
                .into(),
        );
    }
    for pattern in patterns {
        globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
            .map_err(|_| "rule glob is malformed")?;
    }
    Ok(true)
}

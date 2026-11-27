//! Bounded Markdown/frontmatter capture shared by command and skill catalogs.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use yaml_rust2::parser::{Event, Parser};
use yaml_rust2::{Yaml, YamlLoader};

const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_FRONTMATTER_BYTES: usize = 32 * 1024;
const MAX_SCAN_ENTRIES: usize = 2048;
const MAX_SCAN_DEPTH: usize = 16;

pub(super) struct Markdown {
    pub metadata: Yaml,
    pub content: String,
}

pub(super) fn read(path: &Path) -> Result<Markdown, String> {
    let file =
        File::open(path).map_err(|error| format!("catalog Markdown open failed: {error}"))?;
    let mut bytes = Vec::new();
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("catalog Markdown read failed: {error}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("catalog Markdown exceeds 256 KiB".to_owned());
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| "catalog Markdown is not UTF-8".to_owned())?;
    parse(text)
}

pub(super) fn parse(text: &str) -> Result<Markdown, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().unwrap_or("");
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return Ok(Markdown {
            metadata: Yaml::Hash(Default::default()),
            content: text.to_owned(),
        });
    }
    let start = first.len();
    let mut end = start;
    let mut boundary = None;
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            boundary = Some((end, end + line.len()));
            break;
        }
        end += line.len();
        if end - start > MAX_FRONTMATTER_BYTES {
            return Err("catalog frontmatter exceeds 32 KiB".to_owned());
        }
    }
    let (end, content) =
        boundary.ok_or_else(|| "catalog frontmatter has no closing delimiter".to_owned())?;
    let raw = &text[start..end];
    if raw.trim().is_empty() {
        return Ok(Markdown {
            metadata: Yaml::Hash(Default::default()),
            content: text[content..].to_owned(),
        });
    }
    // Check nesting and events before the allocating document loader. Alias
    // expansion is explicitly refused; it cannot multiply the capture bound.
    let mut parser = Parser::new_from_str(raw);
    let mut depth = 0usize;
    let mut ended = false;
    for _ in 0..2048 {
        let (event, _) = parser
            .next_token()
            .map_err(|_| "catalog frontmatter is malformed".to_owned())?;
        match event {
            Event::StreamEnd => {
                ended = true;
                break;
            }
            Event::Alias(_) => {
                return Err("catalog frontmatter aliases are not admitted".to_owned())
            }
            Event::MappingStart(_, _) | Event::SequenceStart(_, _) => {
                depth += 1;
                if depth > MAX_SCAN_DEPTH {
                    return Err("catalog frontmatter nesting exceeds 16".to_owned());
                }
            }
            Event::MappingEnd | Event::SequenceEnd => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| "catalog frontmatter depth is invalid".to_owned())?;
            }
            _ => {}
        }
    }
    if !ended {
        return Err("catalog frontmatter exceeds 2048 parser events".to_owned());
    }
    let mut docs = YamlLoader::load_from_str(raw)
        .map_err(|_| "catalog frontmatter is malformed or has duplicate keys".to_owned())?;
    if docs.len() != 1 {
        return Err("catalog frontmatter must contain exactly one document".to_owned());
    }
    let metadata = docs
        .pop()
        .ok_or_else(|| "catalog frontmatter document is absent".to_owned())?;
    if metadata.as_hash().is_none() {
        return Err("catalog frontmatter root must be a mapping".to_owned());
    }
    Ok(Markdown {
        metadata,
        content: text[content..].to_owned(),
    })
}

pub(super) fn scan(root: &Path, skills: bool) -> Result<Vec<PathBuf>, String> {
    scan_tree(root, skills, &mut |_| Ok(()))
}

pub(super) fn watch_directories(
    root: &Path,
    before_scan: &mut impl FnMut(&Path) -> Result<(), String>,
) -> Result<(), String> {
    scan_tree(root, true, before_scan).map(|_| ())
}

fn scan_tree(
    root: &Path,
    skills: bool,
    before_scan: &mut impl FnMut(&Path) -> Result<(), String>,
) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut visited = BTreeSet::new();
    let mut files = Vec::new();
    let mut entries = 0;
    while let Some((path, depth)) = pending.pop() {
        let canonical = match std::fs::canonicalize(&path) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && depth == 0 => continue,
            Err(error) => return Err(format!("catalog directory resolution failed: {error}")),
        };
        if !visited.insert(canonical.clone()) {
            continue;
        }
        // Install native coverage before enumerating. A child created during
        // registration is then either in this scan or in the watched events.
        before_scan(&canonical)?;
        let directory = std::fs::read_dir(&path)
            .map_err(|error| format!("catalog directory scan failed: {error}"))?;
        for entry in directory {
            entries += 1;
            if entries > MAX_SCAN_ENTRIES {
                return Err("catalog scan exceeds 2048 entries".to_owned());
            }
            let entry =
                entry.map_err(|error| format!("catalog directory entry unreadable: {error}"))?;
            let child = entry.path();
            if child.as_os_str().len() > 4096 {
                return Err("catalog scan path exceeds its bound".to_owned());
            }
            let metadata = std::fs::metadata(&child)
                .map_err(|error| format!("catalog entry probe failed: {error}"))?;
            if metadata.is_dir() {
                if depth == MAX_SCAN_DEPTH {
                    return Err("catalog scan depth exceeds 16".to_owned());
                }
                pending.push((child, depth + 1));
            } else if metadata.is_file()
                && child.extension().is_some_and(|extension| extension == "md")
                && (!skills
                    || depth == 0
                    || child.file_name().is_some_and(|name| name == "SKILL.md"))
            {
                files.push(child);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_preserves_multiline_text_and_refuses_unbounded_or_ambiguous_input() {
        let doc = parse("---\nname: sample\ndescription: |\n  first line\n  second line\nmetadata:\n  grapt/slash: false\n---\nBody\ttext\n")
            .unwrap_or_else(|error| panic!("frontmatter decode failed: {error}"));
        assert_eq!(
            doc.metadata["description"].as_str(),
            Some("first line\nsecond line\n")
        );
        assert_eq!(
            doc.metadata["metadata"]["grapt/slash"].as_bool(),
            Some(false)
        );
        assert_eq!(doc.content, "Body\ttext\n");
        for text in [
            "---\nname: first\nname: second\n---\n",
            "---\na: &a [x]\nb: *a\n---\n",
            "---\nname: absent close\n",
        ] {
            assert!(
                parse(text).is_err(),
                "invalid frontmatter became a successful catalog entry"
            );
        }
        assert!(parse(&format!(
            "---\nx: {}0{}\n---\n",
            "[".repeat(17),
            "]".repeat(17)
        ))
        .is_err());
    }
}

//! Grapt admits bounded tool-name alternatives and globs, not arbitrary regex.
//! Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
//! Licensed under the Apache License, Version 2.0; see LICENSE-APACHE.
//! Provided AS IS, without warranties or conditions of any kind.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Matcher(String);

impl Matcher {
    pub(super) fn new(pattern: &str) -> Result<Self, String> {
        if pattern.len() > 1024
            || pattern.split('|').count() > 32
            || !pattern
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.*?|-/".contains(&b))
        {
            return Err("hook matcher must be at most 1024 bytes of tool-name globs".into());
        }
        Ok(Self(pattern.into()))
    }

    pub(super) fn matches(&self, name: &str) -> bool {
        if name.len() > 1024 {
            return false;
        }
        self.0.is_empty()
            || self.0.split('|').any(|pattern| {
                glob(pattern.as_bytes(), name.as_bytes())
                    || aliases(name)
                        .iter()
                        .any(|alias| glob(pattern.as_bytes(), alias.as_bytes()))
            })
    }
}

pub(in crate::dogfood) fn aliases(name: &str) -> &'static [&'static str] {
    match name {
        "shell" | "exec_command" | "write_stdin" | "request_sandbox_access" => &["Bash"],
        "edit" | "apply_patch" | "patch" => &["Edit"],
        "write" => &["Write"],
        "read" | "read_lines" => &["Read", "NotebookRead"],
        "list" => &["LS"],
        "web_fetch" => &["WebFetch"],
        "websearch" => &["WebSearch"],
        "update_plan" => &["TodoWrite"],
        "view_image" => &["Read"],
        "glob" => &["Glob"],
        "grep" => &["Grep"],
        "spawn_agent" | "subagent" => &["Agent"],
        _ => &[],
    }
}

// Dynamic programming bounds work by pattern bytes times tool-name bytes;
// neither recursion nor regex expansion depends on caller-controlled input.
fn glob(pattern: &[u8], name: &[u8]) -> bool {
    let mut previous = vec![false; name.len() + 1];
    previous[0] = true;
    for &byte in pattern {
        let mut next = vec![false; name.len() + 1];
        next[0] = byte == b'*' && previous[0];
        for index in 0..name.len() {
            next[index + 1] = if byte == b'*' {
                previous[index + 1] || next[index]
            } else {
                previous[index] && (byte == b'?' || byte == name[index])
            };
        }
        previous = next;
    }
    previous[name.len()]
}

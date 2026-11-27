//! Builtin failure diagnostics: redact before bounding and keep the cause first.
use super::*;

pub(in crate::dogfood) const MAX_BYTES: usize = 2048;
const CAUSE_BYTES: usize = 640;
const STDERR_BYTES: usize = 1024;

fn builtin(tool: &str) -> bool {
    matches!(
        tool,
        SHELL_TOOL | READ_TOOL | terminal::EXEC_COMMAND | sandbox_access::TOOL
    )
}

pub(super) fn format(tool: &str, preparation: bool, raw: &str, credentials: &[String]) -> String {
    format_protected(tool, preparation, raw, protection(credentials))
}

fn format_protected(
    tool: &str,
    preparation: bool,
    raw: &str,
    protection: Result<crate::dogfood::mcp::stderr_tail::Tail, &'static str>,
) -> String {
    if !builtin(tool) {
        return raw.to_owned();
    }
    if let (Some(actual), Some(receipt)) = (sandbox::Receipt::parse(raw), sandbox::receipt(raw)) {
        let detail = receipt
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("sandbox refused; no fallback ran");
        let mut enforcement = sandbox::value(&actual.enforcement);
        if let Some(reason) = enforcement
            .get("reason")
            .and_then(serde_json::Value::as_str)
        {
            let redacted = redact(reason, &protection);
            enforcement["reason"] = serde_json::json!(encoded_slice(&redacted, CAUSE_BYTES, false));
        }
        let envelope = serde_json::json!({"enforcement":enforcement,"ran":actual.ran,"error":""});
        return diagnostic(tool, preparation, detail, &protection, Some(envelope));
    }
    diagnostic(tool, preparation, raw, &protection, None)
}

fn diagnostic(
    tool: &str,
    preparation: bool,
    raw: &str,
    protection: &Result<crate::dogfood::mcp::stderr_tail::Tail, &'static str>,
    envelope: Option<serde_json::Value>,
) -> String {
    let class = if preparation {
        "preparation"
    } else if raw.starts_with("shell timed out after ") {
        "timeout"
    } else if raw.starts_with("shell exited with ") {
        "exit"
    } else if raw.contains("spawn failed") || raw.contains("startup") {
        "spawn"
    } else if tool == READ_TOOL {
        "read"
    } else {
        "execution"
    };
    let (cause, stderr) = raw
        .split_once("\n[stderr]\n")
        .unwrap_or((raw, "not observed"));
    let cause = cause
        .split_once("; captured output:")
        .map_or(cause, |(cause, _)| cause);
    let cause = redact_with(cause, protection, true);
    let stderr = redact(stderr, protection);
    let cause = head(&cause, CAUSE_BYTES);
    let stderr = tail(&stderr, STDERR_BYTES);
    let next = if preparation {
        "Correct the arguments; this call did not execute."
    } else if tool == READ_TOOL {
        "Check the path and file format, then adjust the read window."
    } else {
        "Check the reported effect before retrying; use exec_command for long-running work."
    };
    let render = |cause: &str, stderr: &str| {
        format!("{tool} failed ({class})\nCause: {cause}\nStderr tail: {stderr}\nNext: {next}")
    };
    let Some(mut envelope) = envelope else {
        return render(cause, stderr);
    };
    // Reserve the immutable receipt, section labels and complete guidance first.
    // Each variable section then spends its actual JSON-escaped byte width.
    envelope["error"] = serde_json::json!(render("", ""));
    let available = MAX_BYTES.saturating_sub(envelope.to_string().len());
    let stderr_reserve = stderr
        .chars()
        .map(escaped)
        .sum::<usize>()
        .min(available / 2);
    let cause = encoded_slice(cause, available - stderr_reserve, false);
    let stderr = encoded_slice(
        stderr,
        available - cause.chars().map(escaped).sum::<usize>(),
        true,
    );
    envelope["error"] = serde_json::json!(render(cause, stderr));
    envelope.to_string()
}

fn escaped(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if c <= '\u{1f}' => 6,
        c => c.len_utf8(),
    }
}

fn encoded_slice(text: &str, budget: usize, from_end: bool) -> &str {
    let mut characters = text.char_indices();
    let mut used = 0;
    let mut boundary = if from_end { text.len() } else { 0 };
    while let Some((offset, character)) = if from_end {
        characters.next_back()
    } else {
        characters.next()
    } {
        let cost = escaped(character);
        if cost > budget.saturating_sub(used) {
            break;
        }
        used += cost;
        boundary = if from_end {
            offset
        } else {
            offset + character.len_utf8()
        };
    }
    if from_end {
        &text[boundary..]
    } else {
        &text[..boundary]
    }
}

pub(in crate::dogfood) fn public_detail<'a>(
    tool: &str,
    action: Option<&str>,
    local: bool,
    detail: &'a str,
) -> Option<&'a str> {
    if !builtin(tool)
        || !local
        || detail.len() > MAX_BYTES
        || detail.chars().any(|c| c.is_control() && c != '\n')
    {
        return None;
    }
    if matches!(
        tool,
        SHELL_TOOL | terminal::EXEC_COMMAND | sandbox_access::TOOL
    ) && sandbox::receipt(detail).is_some()
    {
        return Some(detail);
    }
    let header = detail.lines().next()?;
    let class = header
        .strip_prefix(tool)?
        .strip_prefix(" failed (")?
        .strip_suffix(')')?;
    if !matches!(
        class,
        "preparation" | "timeout" | "exit" | "spawn" | "read" | "execution"
    ) {
        return None;
    }
    let owned = match (tool, action) {
        (READ_TOOL, Some("read" | "list"))
        | (SHELL_TOOL | terminal::EXEC_COMMAND | sandbox_access::TOOL, Some("shell")) => true,
        (sandbox_access::TOOL, Some(grapt_permission::SANDBOX_WRITE_ACTION)) => true,
        // Auto-allow does not create a permission-question fact. Dispatch owns
        // these reserved builtin names before considering plugin/MCP routes.
        (_, None) => true,
        _ => false,
    };
    (owned
        && detail.contains("\nCause: ")
        && detail.contains("\nStderr tail: ")
        && detail.contains("\nNext: "))
    .then_some(detail)
}

fn head(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn tail(text: &str, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn protection(
    credentials: &[String],
) -> Result<crate::dogfood::mcp::stderr_tail::Tail, &'static str> {
    let mut protection = crate::dogfood::mcp::stderr_tail::Tail::default();
    let mut bytes = 0usize;
    for (index, (name, value)) in std::env::vars_os().enumerate() {
        bytes = bytes.saturating_add(name.len()).saturating_add(value.len());
        if index >= 1024 || bytes > 256 * 1024 {
            return Err("[diagnostic withheld: environment bound]");
        }
        if name
            .to_str()
            .is_some_and(|name| shell_environment::secret_name(name, credentials))
        {
            let Some(value) = value.to_str() else {
                return Err("[diagnostic withheld: private value encoding]");
            };
            protection.protect(value);
        }
    }
    Ok(protection)
}

pub(super) struct Stderr {
    protection: Result<crate::dogfood::mcp::stderr_tail::Tail, &'static str>,
    observed: usize,
}

impl Stderr {
    pub(super) fn new(credentials: &[String]) -> Self {
        Self {
            protection: protection(credentials),
            observed: 0,
        }
    }
    pub(super) fn push(&mut self, bytes: &[u8]) {
        self.observed = self.observed.saturating_add(bytes.len());
        if let Ok(tail) = &mut self.protection {
            tail.push(bytes);
        }
    }
    pub(super) fn text(mut self) -> String {
        match &mut self.protection {
            Err(reason) => (*reason).to_owned(),
            Ok(tail) => {
                tail.finish();
                let text = tail.text_lines();
                // An evicted credential label must not leave its unlabelled
                // value visible. Drop the first partial line before publishing.
                if self.observed > 4096 {
                    let text = text.split_once('\n').map_or("", |(_, complete)| complete);
                    format!("[earlier stderr omitted]\n{text}")
                } else {
                    text
                }
            }
        }
    }
}

pub(super) fn sanitise(text: &str, private: &[&str]) -> String {
    let mut protection = protection(&[]);
    if let Ok(tail) = &mut protection {
        for value in private {
            tail.protect(value);
        }
    }
    redact_with(text, &protection, true)
}

fn redact(
    text: &str,
    protection: &Result<crate::dogfood::mcp::stderr_tail::Tail, &'static str>,
) -> String {
    redact_with(text, protection, false)
}

fn redact_with(
    text: &str,
    protection: &Result<crate::dogfood::mcp::stderr_tail::Tail, &'static str>,
    prefix: bool,
) -> String {
    let protection = match protection {
        Ok(protection) => protection,
        Err(reason) => return (*reason).to_owned(),
    };
    // Mask unknown but labelled/token-shaped credentials too. Each input is
    // already limited by the tool adapter; replacement precedes tail eviction.
    let mut safe = text.to_owned();
    for pattern in [
        r#"(?i)\b[a-z0-9_-]*(?:authorization|api[_-]?key|token|password|secret|credential|cookie)[a-z0-9_-]*[\s"']*[:=][\s"']*[^\r\n,}\]]+"#,
        r"(?i)\b(?:bearer|basic)\s+[^\s\r\n,}\]]+",
        r"(?i)\b(?:sk-|sk_|ghp_|github_pat_|xox[baprs]-)[a-z0-9_\\-]+",
        r"://[^/\s:@]+:[^/@\s]+@",
    ] {
        let Ok(pattern) = regex::Regex::new(pattern) else {
            return "[diagnostic withheld: redaction unavailable]".into();
        };
        safe = pattern.replace_all(&safe, "[private]").into_owned();
    }
    if prefix {
        protection.sanitise_prefix(&safe)
    } else {
        protection.sanitise_lines(&safe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_stream_redacts_before_eviction_and_keeps_the_actual_end() {
        let secret = format!("private-{}-end", "S".repeat(6000));
        let mut private = crate::dogfood::mcp::stderr_tail::Tail::default();
        private.protect(&secret);
        let mut stderr = Stderr {
            protection: Ok(private),
            observed: 0,
        };
        stderr.push(&vec![b'x'; 600_000]);
        for chunk in secret.as_bytes().chunks(37) {
            stderr.push(chunk);
        }
        stderr.push(b"\nactual last stderr line\n");
        let text = stderr.text();
        assert!(text.ends_with("actual last stderr line"), "{text}");
        assert!(!text.contains("SSSS") && !text.contains("private-"));
        assert!(text.len() < 4200);
        let mut labelled = Stderr {
            protection: Ok(crate::dogfood::mcp::stderr_tail::Tail::default()),
            observed: 0,
        };
        labelled.push(b"api_key=");
        labelled.push(&vec![b'K'; 8000]);
        labelled.push(b"\nlast complete line\n");
        let text = labelled.text();
        assert!(!text.contains("KKK"));
        assert!(text.contains("last complete line"));
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn census_native_failures_keep_cause_exit_and_stderr_without_credentials() {
        for (tool, raw, class, cause) in [
            (
                SHELL_TOOL,
                "shell exited with exit status: 7; captured output: ignored\n[stderr]\nlink failed",
                "exit",
                "exit status: 7",
            ),
            (
                SHELL_TOOL,
                "shell timed out after 30000 ms; captured output: ignored\n[stderr]\nstill waiting",
                "timeout",
                "30000 ms",
            ),
            (
                terminal::EXEC_COMMAND,
                "terminal spawn failed: executable unavailable",
                "spawn",
                "executable unavailable",
            ),
            (
                READ_TOOL,
                "read content is not UTF-8 at line 60",
                "read",
                "line 60",
            ),
        ] {
            let detail = format_protected(
                tool,
                false,
                raw,
                Ok(crate::dogfood::mcp::stderr_tail::Tail::default()),
            );
            assert!(detail.contains(&format!("({class})")) && detail.contains(cause));
            assert_eq!(
                public_detail(
                    tool,
                    Some(if tool == READ_TOOL { "read" } else { "shell" }),
                    true,
                    &detail
                ),
                Some(detail.as_str())
            );
            assert_eq!(
                public_detail("mcp_shell", Some("shell"), true, &detail),
                None
            );
            assert_eq!(
                public_detail(tool, None, true, &detail),
                Some(detail.as_str())
            );
            assert_eq!(
                public_detail(tool, Some("mcp_foreign"), true, &detail),
                None
            );
            assert_eq!(public_detail(tool, Some("shell"), false, &detail), None);
        }
        let raw = format!("shell timed out after 1 ms; captured output: ignored\n[stderr]\n{}\nAuthorization: Bearer PRIVATE_AUTH\napi_key=PRIVATE_KEY\n{}\nlast failure", "x".repeat(9000), "語".repeat(900));
        let detail = format_protected(
            SHELL_TOOL,
            false,
            &raw,
            Ok(crate::dogfood::mcp::stderr_tail::Tail::default()),
        );
        assert!(detail.len() <= MAX_BYTES);
        assert!(detail.contains("1 ms") && detail.contains("last failure"));
        assert!(!detail.contains("PRIVATE_AUTH") && !detail.contains("PRIVATE_KEY"));
        assert!(!detail.chars().any(|c| c.is_control() && c != '\n'));
        // The same diagnostic must survive sealing, including expensive JSON
        // escaping and a degraded reason competing for the failure byte limit.
        for enforcement in [
            RealizedEnforcement::HarnessOnly,
            RealizedEnforcement::PlatformSandbox {
                backend: "mxc".into(),
            },
            RealizedEnforcement::Degraded {
                backend: "mxc".into(),
                reason: format!(
                    "API unavailable {}\napi_key=PRIVATE_BACKEND",
                    "\"\\界\n".repeat(900)
                ),
            },
        ] {
            for cause in [String::new(), "\"\\界".repeat(900)] {
                let raw = raw.replacen(
                    "; captured output:",
                    &format!(" {cause}; captured output:"),
                    1,
                );
                let sealed = sandbox::failure(&enforcement, &raw, None);
                let detail = format_protected(
                    SHELL_TOOL,
                    false,
                    &sealed,
                    Ok(crate::dogfood::mcp::stderr_tail::Tail::default()),
                );
                assert!(
                    detail.len() <= MAX_BYTES,
                    "encoded failure receipt exceeded bound"
                );
                let receipt = sandbox::receipt(&detail)
                    .unwrap_or_else(|| panic!("sealed failure receipt was malformed"));
                let error = receipt["error"]
                    .as_str()
                    .unwrap_or_else(|| panic!("sealed failure diagnostic missing"));
                assert!(error.starts_with("shell failed (timeout)\nCause: "));
                assert!(error.contains("1 ms"), "sealed receipt lost cause");
                assert!(
                    error.contains("last failure"),
                    "sealed receipt lost stderr end"
                );
                assert!(
                    error.ends_with("use exec_command for long-running work."),
                    "sealed receipt lost Next guidance"
                );
                assert!(
                    !detail.contains("PRIVATE_AUTH")
                        && !detail.contains("PRIVATE_KEY")
                        && !detail.contains("PRIVATE_BACKEND")
                );
                assert_eq!(
                    receipt["enforcement"]["kind"],
                    sandbox::value(&enforcement)["kind"]
                );
                assert!(receipt["ran"].is_null());
            }
        }
    }
}

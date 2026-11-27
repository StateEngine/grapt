//! The import command's public seam: grammar, help, and the empty-source
//! finding, which must report without contacting a service.
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static FIXTURE_INDEX: AtomicUsize = AtomicUsize::new(0);

struct ScratchDir(PathBuf);

impl Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for ScratchDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh_directory(label: &str) -> Result<ScratchDir, Box<dyn std::error::Error>> {
    let pid = std::process::id();
    loop {
        let index = FIXTURE_INDEX.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("grapt-import-seam-{pid}-{label}-{index}"));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(ScratchDir(path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

#[tokio::test]
async fn import_public_binary_exposes_help_and_grammar() -> Result<(), Box<dyn std::error::Error>> {
    for (arguments, code, fragment) in [
        (vec!["import", "--help"], 0, "import mcp"),
        (vec!["import"], 2, "import supports mcp"),
        (vec!["import", "mcp"], 2, "--from"),
        (
            vec!["import", "mcp", "--from", "gemini", "--service", "/profile"],
            2,
            "--from",
        ),
        (
            vec![
                "import",
                "openai",
                "--from",
                "claude",
                "--service",
                "/profile",
            ],
            2,
            "import supports mcp",
        ),
    ] {
        let rendered = format!("{arguments:?}");
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
                .args(arguments)
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        assert_eq!(output.status.code(), Some(code), "{rendered}");
        let mut text = String::from_utf8(output.stdout)?;
        text.push_str(&String::from_utf8(output.stderr)?);
        assert!(text.contains(fragment), "{text}");
    }
    Ok(())
}

#[tokio::test]
async fn import_without_source_files_reports_and_needs_no_service(
) -> Result<(), Box<dyn std::error::Error>> {
    for source in ["claude", "codex"] {
        let home = fresh_directory(&format!("home-{source}"))?;
        let directory = fresh_directory(&format!("project-{source}"))?;
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
                .args([
                    "import",
                    "mcp",
                    "--from",
                    source,
                    "--service",
                    "/no-such-import-root",
                    "--directory",
                    directory.to_str().ok_or("fixture path is not UTF-8")?,
                ])
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        assert_eq!(output.status.code(), Some(0), "{source}");
        let text = String::from_utf8(output.stdout)?;
        assert!(text.contains("No MCP servers found"), "{text}");
    }
    Ok(())
}

async fn run_import(
    source: &str,
    home: &std::path::Path,
    directory: &std::path::Path,
) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    // /no-such-import-root doubles as the no-service-contact witness: any
    // path that reaches discovery fails loudly instead of succeeding.
    Ok(tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
            .args([
                "import",
                "mcp",
                "--from",
                source,
                "--service",
                "/no-such-import-root",
                "--directory",
                directory.to_str().ok_or("fixture path is not UTF-8")?,
            ])
            .env("HOME", home)
            .env("USERPROFILE", home)
            .kill_on_drop(true)
            .output(),
    )
    .await??)
}

fn diagnostics(output: &std::process::Output) -> Result<String, Box<dyn std::error::Error>> {
    Ok(String::from_utf8(output.stderr.to_vec())?)
}

#[tokio::test]
async fn import_refuses_malformed_source_documents() -> Result<(), Box<dyn std::error::Error>> {
    // A present container with the wrong type is a broken source, not an
    // empty one: the import must name the break, not report "No MCP
    // servers found".
    let home = fresh_directory("claude-container-home")?;
    std::fs::write(home.join(".claude.json"), b"{\"mcpServers\": [\"docs\"]}")?;
    let output = run_import("claude", &home, &fresh_directory("claude-container")?).await?;
    assert_eq!(
        output.status.code(),
        Some(2),
        "malformed mcpServers must fail"
    );
    let text = diagnostics(&output)?;
    assert!(text.contains("mcpServers must be an object"), "{text}");

    // A source that is valid JSON but not an object is broken the same way.
    let home = fresh_directory("claude-shape-home")?;
    std::fs::write(home.join(".claude.json"), b"[\"not\", \"an\", \"object\"]")?;
    let output = run_import("claude", &home, &fresh_directory("claude-shape")?).await?;
    assert_eq!(output.status.code(), Some(2), "non-object JSON must fail");
    let text = diagnostics(&output)?;
    assert!(text.contains("the source is not a JSON object"), "{text}");

    let home = fresh_directory("codex-container-home")?;
    std::fs::create_dir_all(home.join(".codex"))?;
    std::fs::write(home.join(".codex/config.toml"), b"mcp_servers = \"docs\"\n")?;
    let output = run_import("codex", &home, &fresh_directory("codex-container")?).await?;
    assert_eq!(
        output.status.code(),
        Some(2),
        "malformed mcp_servers must fail"
    );
    let text = diagnostics(&output)?;
    assert!(text.contains("mcp_servers must be a table"), "{text}");

    // A source past the byte bound is refused before any of it is read.
    let home = fresh_directory("oversize-home")?;
    std::fs::create_dir_all(home.join(".codex"))?;
    let oversized = home.join(".codex/config.toml");
    let file = std::fs::File::create(&oversized)?;
    file.set_len(16 * 1024 * 1024 + 1)?;
    let output = run_import("codex", &home, &fresh_directory("oversize")?).await?;
    assert_eq!(
        output.status.code(),
        Some(2),
        "an oversize source must fail"
    );
    let text = diagnostics(&output)?;
    assert!(
        text.contains("exceeds the 16777216-byte import bound"),
        "{text}"
    );
    Ok(())
}

#[tokio::test]
async fn import_refuses_entries_it_cannot_translate_and_needs_no_service(
) -> Result<(), Box<dyn std::error::Error>> {
    // Every entry refused: the import succeeds, names each refusal, and
    // never touches the service root.
    let home = fresh_directory("codex-refused-home")?;
    std::fs::create_dir_all(home.join(".codex"))?;
    std::fs::write(
        home.join(".codex/config.toml"),
        b"[mcp_servers.audit]\ncommand = \"audit\"\nbearer_token_env_var = \"AUDIT_TOKEN\"\n",
    )?;
    let output = run_import("codex", &home, &fresh_directory("codex-refused")?).await?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "an all-refused import must succeed"
    );
    let text = diagnostics(&output)?;
    let finding = String::from_utf8(output.stdout)?;
    assert!(finding.contains("no importable servers"), "{finding}");
    assert!(
        text.contains(
            "refused audit: codex entry declares bearer_token_env_var, which the import does not support"
        ),
        "{text}"
    );

    let home = fresh_directory("claude-refused-home")?;
    std::fs::write(
        home.join(".claude.json"),
        b"{\"mcpServers\":{\"legacy\":{\"type\":\"ws\",\"url\":\"wss://legacy.example\"}}}",
    )?;
    let output = run_import("claude", &home, &fresh_directory("claude-refused")?).await?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "an all-refused import must succeed"
    );
    let text = diagnostics(&output)?;
    let finding = String::from_utf8(output.stdout)?;
    assert!(finding.contains("no importable servers"), "{finding}");
    assert!(
        text.contains("refused legacy: transport is not supported"),
        "{text}"
    );
    Ok(())
}

#[tokio::test]
async fn import_refuses_a_present_non_string_type_and_needs_no_service(
) -> Result<(), Box<dyn std::error::Error>> {
    // A `type` key that is present but not a string is a broken definition,
    // refused naming the field; only an absent type opens inference.
    let home = fresh_directory("claude-badtype-home")?;
    std::fs::write(
        home.join(".claude.json"),
        b"{\"mcpServers\":{\"n\":{\"type\":42,\"command\":\"echo\"}}}",
    )?;
    let output = run_import("claude", &home, &fresh_directory("claude-badtype")?).await?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "an all-refused import must succeed"
    );
    let text = diagnostics(&output)?;
    let finding = String::from_utf8(output.stdout)?;
    assert!(finding.contains("no importable servers"), "{finding}");
    assert!(text.contains("refused n: type must be a string"), "{text}");
    Ok(())
}

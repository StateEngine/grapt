//! Public command behaviour against disposable vendor configuration.
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);

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

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> Result<ScratchDir, Box<dyn std::error::Error>> {
    let pid = std::process::id();
    let root = loop {
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("grapt-import-files-{pid}-{index}"));
        match std::fs::create_dir(&path) {
            Ok(()) => break ScratchDir(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    std::fs::create_dir_all(root.join("home"))?;
    std::fs::create_dir_all(root.join("project"))?;
    Ok(root)
}
async fn run(
    root: &Path,
    kind: &str,
    source: &str,
    apply: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
    command
        .args(["import", kind, "--from", source, "--directory"])
        .arg(root.join("project"))
        .env("HOME", root.join("home"))
        .env("USERPROFILE", root.join("home"))
        .env("GRAPT_CONFIG_DIR", root.join("config"))
        .kill_on_drop(true);
    if apply {
        command.arg("--apply");
    }
    let output =
        tokio::time::timeout(std::time::Duration::from_secs(10), command.output()).await??;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

#[tokio::test]
async fn instructions_preview_apply_and_repeat_preserve_existing_sources_and_path_scope(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = fixture()?;
    let project = root.join("project");
    std::fs::create_dir_all(project.join(".claude/rules/nested"))?;
    std::fs::write(
        project.join("CLAUDE.md"),
        "Project instructions\n@docs/style.md\n",
    )?;
    std::fs::write(project.join("CLAUDE.local.md"), "Local instructions\n")?;
    std::fs::create_dir_all(project.join("docs"))?;
    std::fs::write(project.join("docs/style.md"), "Project style\n")?;
    std::fs::write(project.join(".claude/shared.md"), "Rule reference\n")?;
    std::fs::write(
        project.join(".claude/rules/rust.md"),
        "---\npaths: [\"src/**/*.rs\"]\n---\nRust only\n@../shared.md\n",
    )?;
    std::fs::write(
        project.join(".claude/rules/nested/web.md"),
        "---\npaths: \"web/**\"\n---\nWeb only\n",
    )?;
    std::fs::create_dir_all(root.join("home/.claude/docs"))?;
    std::fs::write(
        root.join("home/.claude/CLAUDE.md"),
        "Global instructions\n@docs/global.md\n",
    )?;
    std::fs::write(
        root.join("home/.claude/docs/global.md"),
        "Global reference\n",
    )?;
    let preview = run(&root, "instructions", "claude", false).await?;
    assert!(preview.contains("dry run"));
    assert!(!project.join("AGENTS.md").exists());
    run(&root, "instructions", "claude", true).await?;
    assert_eq!(
        std::fs::read_to_string(root.join("config/.grapt/imports/claude/docs/global.md"))?,
        "Global reference\n"
    );
    let agents = std::fs::read_to_string(project.join("AGENTS.md"))?;
    assert!(agents.contains("@CLAUDE.md"));
    assert!(agents.contains("@CLAUDE.local.md"));
    let rule = std::fs::read_to_string(project.join(".grapt/rules/rust.md"))?;
    assert!(rule.contains("paths: [\"src/**/*.rs\"]"));
    assert!(rule.contains("@../../.claude/rules/../shared.md"));
    std::fs::write(project.join("AGENTS.md"), "Existing instructions win\n")?;
    let repeated = run(&root, "instructions", "claude", true).await?;
    assert!(repeated.contains("side by side"));
    assert_eq!(
        std::fs::read_to_string(project.join("AGENTS.md"))?,
        "Existing instructions win\n"
    );
    assert!(run(&root, "instructions", "codex", false)
        .await?
        .contains("already used"));
    assert_eq!(
        std::fs::read_to_string(project.join("CLAUDE.local.md"))?,
        "Local instructions\n"
    );
    Ok(())
}

#[tokio::test]
async fn skills_keep_native_roots_and_copy_codex_resources_without_clobbering(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = fixture()?;
    let project = root.join("project");
    for directory in [
        project.join(".claude/skills/native"),
        root.join("home/.claude/skills/global"),
        project.join(".codex/skills/example/scripts"),
        project.join(".codex/skills/example/ABC"),
    ] {
        std::fs::create_dir_all(directory)?;
    }
    std::fs::write(
        project.join(".codex/skills/example/SKILL.md"),
        "---\nname: example\ndescription: Example\n---\nUse scripts/check.py\n",
    )?;
    std::fs::write(
        project.join(".codex/skills/example/scripts/check.py"),
        "print('example')\n",
    )?;
    let nested =
        "---\nname: nested-example\ndescription: Nested resource\n---\nKeep nested resources\n";
    std::fs::write(project.join(".codex/skills/example/ABC/SKILL.md"), nested)?;
    assert!(run(&root, "skills", "claude", false)
        .await?
        .contains("already used"));
    run(&root, "skills", "codex", false).await?;
    assert!(!project.join(".agents").exists());
    run(&root, "skills", "codex", true).await?;
    let script = project.join(".agents/skills/example/scripts/check.py");
    assert_eq!(std::fs::read_to_string(&script)?, "print('example')\n");
    std::fs::write(&script, "Existing resource\n")?;
    assert_eq!(
        std::fs::read_to_string(project.join(".agents/skills/example/ABC/SKILL.md"))?,
        nested
    );
    // A new nested source must not modify an enclosing native skill even when
    // the vendor no longer declares its outer skill.
    std::fs::remove_file(project.join(".codex/skills/example/SKILL.md"))?;
    std::fs::create_dir_all(project.join(".codex/skills/example/NEW"))?;
    std::fs::write(project.join(".codex/skills/example/NEW/SKILL.md"), nested)?;
    assert!(run(&root, "skills", "codex", true)
        .await?
        .contains("kept existing skill"));
    assert_eq!(std::fs::read_to_string(script)?, "Existing resource\n");
    assert!(!project.join(".agents/skills/example/NEW").exists());
    Ok(())
}

#[tokio::test]
async fn generated_instruction_graph_rejects_native_overflow_before_publication_and_keeps_fences(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = fixture()?;
    let global = root.join("home/.claude");
    std::fs::create_dir_all(&global)?;
    std::fs::write(global.join("CLAUDE.md"), "@large.md\n")?;
    std::fs::write(global.join("large.md"), "x".repeat(65_537))?;
    for phase in 0..3 {
        if phase == 1 {
            // Legal source size, but the generated wrapper and heading overflow.
            std::fs::write(global.join("large.md"), "x".repeat(65_530))?;
        } else if phase == 2 {
            // Each ambient root is legal alone; their common wrapper is not.
            std::fs::write(global.join("CLAUDE.md"), "x".repeat(40_000))?;
            std::fs::create_dir_all(global.join("rules"))?;
            std::fs::write(global.join("rules/ambient.md"), "y".repeat(40_000))?;
        }
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
            .args([
                "import",
                "instructions",
                "--from",
                "claude",
                "--apply",
                "--directory",
            ])
            .arg(root.join("project"))
            .env("HOME", root.join("home"))
            .env("USERPROFILE", root.join("home"))
            .env("GRAPT_CONFIG_DIR", root.join("config"))
            .kill_on_drop(true)
            .output()
            .await?;
        assert!(!output.status.success(), "invalid generated graph accepted");
        assert!(
            !root.join("config").exists(),
            "validation wrote a destination"
        );
    }
    std::fs::remove_dir_all(global.join("rules"))?;
    std::fs::write(
        global.join("CLAUDE.md"),
        "````text\n```\n@not-an-import.md\n````\nGlobal instructions\n",
    )?;
    std::fs::write(
        root.join("project/CLAUDE.md"),
        "````text\n```\n@not-an-import.md\n````\nProject instructions\n",
    )?;
    run(&root, "instructions", "claude", true).await?;
    assert!(root.join("project/AGENTS.md").is_file());
    assert!(!global.join("not-an-import.md").exists());
    Ok(())
}

#[tokio::test]
async fn instruction_apply_counts_new_and_retained_native_rules_before_writing(
) -> Result<(), Box<dyn std::error::Error>> {
    for existing in [false, true] {
        let root = fixture()?;
        let project = root.join("project");
        let vendor = project.join(".claude/rules");
        let native = project.join(".grapt/rules");
        std::fs::create_dir_all(&vendor)?;
        if existing {
            std::fs::create_dir_all(&native)?;
        }
        let destination = if existing { &native } else { &vendor };
        for index in 0..64 {
            std::fs::write(
                destination.join(format!("rule-{index}.md")),
                "---\npaths: src/**\n---\nTiny rule\n",
            )?;
        }
        std::fs::write(
            vendor.join("extra.md"),
            "---\npaths: src/**\n---\nNew rule\n",
        )?;
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
            .args([
                "import",
                "instructions",
                "--from",
                "claude",
                "--apply",
                "--directory",
            ])
            .arg(&project)
            .env("HOME", root.join("home"))
            .env("USERPROFILE", root.join("home"))
            .env("GRAPT_CONFIG_DIR", root.join("config"))
            .kill_on_drop(true)
            .output()
            .await?;
        assert!(
            !output.status.success(),
            "native rule overflow was accepted"
        );
        assert!(String::from_utf8(output.stderr)?.contains("64 Markdown files"));
        assert!(!native.join("extra.md").exists());
        std::fs::remove_file(vendor.join("extra.md"))?;
        if existing {
            std::fs::remove_file(native.join("rule-63.md"))?;
            std::fs::write(
                vendor.join("extra.md"),
                "---\npaths: src/**\n---\nNew rule\n",
            )?;
        }
        run(&root, "instructions", "claude", true).await?;
        assert_eq!(std::fs::read_dir(&native)?.count(), 64);
    }
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn repeated_global_import_compares_canonical_windows_scopes_without_preview_writes(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = fixture()?;
    let global = root.join("home/.claude");
    std::fs::create_dir_all(global.join("docs"))?;
    std::fs::write(global.join("claude.md"), "@DOCS/SHARED.md\n")?;
    std::fs::write(global.join("docs/shared.md"), "Global instructions\n")?;
    run(&root, "instructions", "claude", false).await?;
    assert!(!root.join("config").exists());
    run(&root, "instructions", "claude", true).await?;
    let retained = std::fs::read(root.join("config/AGENTS.md"))?;
    run(&root, "instructions", "claude", false).await?;
    assert!(!root.join("config/AGENTS.import-claude.md").exists());
    run(&root, "instructions", "claude", true).await?;
    assert_eq!(std::fs::read(root.join("config/AGENTS.md"))?, retained);
    assert!(root.join("config/AGENTS.import-claude.md").is_file());
    Ok(())
}

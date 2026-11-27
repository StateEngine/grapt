use super::*;

#[test]
fn imports_refresh_in_order_and_scoped_rule_edits_leave_ambient_unchanged(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let project = fixture.root.join("project");
    std::fs::create_dir_all(project.join("parts"))?;
    std::fs::write(
        project.join("AGENTS.md"),
        "before\n@parts/one.md\nafter\n```md\n@literal.md\n```\n",
    )?;
    std::fs::write(project.join("parts/one.md"), "first import\n@two.md\n")?;
    std::fs::write(project.join("parts/two.md"), "nested import")?;
    let first = text(&fixture.effective.refresh()?);
    assert!(first.find("before") < first.find("first import"));
    assert!(first.find("first import") < first.find("nested import"));
    assert!(first.find("nested import") < first.find("after"));
    assert!(first.contains("@literal.md"));
    let before = fixture.history()?;
    std::fs::create_dir_all(project.join("sub/.grapt/rules"))?;
    std::fs::write(
        project.join("sub/.grapt/rules/rust.md"),
        "---\nglobs: '**/*.rs'\n---\n@missing.md\n",
    )?;
    assert_eq!(first, text(&fixture.effective.refresh()?));
    assert_eq!(before, fixture.history()?);
    std::fs::write(project.join("parts/two.md"), "updated nested import")?;
    assert!(text(&fixture.effective.refresh()?).contains("updated nested import"));
    assert!(fixture.history()?.through > before.through);
    Ok(())
}

#[test]
fn import_failures_keep_the_last_known_source() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let project = fixture.root.join("project");
    let source = project.join("AGENTS.md");
    std::fs::write(&source, "@good.md\n")?;
    std::fs::write(project.join("good.md"), "retained imported policy")?;
    fixture.effective.refresh()?;
    std::fs::write(project.join("loop.md"), "@AGENTS.md\n")?;
    std::fs::write(project.join("huge.md"), "x".repeat(MAX_FILE_BYTES + 1))?;
    std::fs::write(project.join("wide.md"), "@empty.md\n".repeat(MAX_FILES))?;
    std::fs::write(project.join("empty.md"), "")?;
    std::fs::write(
        project.join("expanded.md"),
        format!("{}\n@good.md", "x".repeat(MAX_FILE_BYTES - 20)),
    )?;
    std::fs::write(fixture.root.join("outside.md"), "must not be imported")?;
    for index in 0..16 {
        std::fs::write(
            project.join(format!("depth{index}.md")),
            format!("@depth{}.md\n", index + 1),
        )?;
    }
    for (content, reason) in [
        ("@missing.md\n", "is missing"),
        ("@loop.md\n", "import cycle"),
        ("@two words\n", "malformed instruction @import"),
        ("@huge.md\n", "exceeds its file bound"),
        ("@expanded.md\n", "expanded instruction source exceeds"),
        ("@wide.md\n", "64 source occurrences"),
        ("@depth0.md\n", "16 levels"),
        ("@../outside.md\n", "resolves outside"),
    ] {
        std::fs::write(&source, content)?;
        let failed = text(&fixture.effective.refresh()?);
        assert!(failed.contains("retained imported policy"));
        assert!(failed.contains(reason), "missing {reason}");
        assert!(!failed.contains("must not be imported"));
    }
    Ok(())
}

#[test]
fn scoped_rule_errors_are_visible_and_discovery_is_bounded(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let root = std::fs::canonicalize(fixture.root.join("project"))?;
    let rules = root.join(".grapt/rules");
    std::fs::create_dir_all(&rules)?;
    let path = rules.join("rule.md");
    for (content, reason) in [
        ("---\nglobs: [\n---\npolicy", "frontmatter"),
        ("---\nglobs: []\n---\npolicy", "1 to 64 patterns"),
        ("---\nglobs: '[broken'\n---\npolicy", "glob is malformed"),
        ("---\nglobs: '**/*.rs'\n---\n@missing.md\n", "is missing"),
    ] {
        std::fs::write(&path, content)?;
        let read = scoped_for_read(&root, &root.join("source.rs"));
        assert!(read.instructions.iter().any(|instruction| instruction
            .content
            .as_ref()
            .is_err_and(|error| error.contains(reason))));
    }
    for index in 0..MAX_FILES {
        std::fs::write(
            rules.join(format!("{index}.md")),
            "---\nglobs: '*.rs'\n---\npolicy",
        )?;
    }
    assert!(scoped_for_read(&root, &root.join("source.rs"))
        .render_text()
        .contains("exceeds 64 Markdown files"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn imports_and_rules_cannot_follow_links_outside_scope() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let root = std::fs::canonicalize(fixture.root.join("project"))?;
    std::fs::write(fixture.root.join("foreign.md"), "foreign policy")?;
    std::os::unix::fs::symlink(fixture.root.join("foreign.md"), root.join("linked.md"))?;
    std::fs::write(root.join("AGENTS.md"), "@linked.md\n")?;
    assert!(fixture.effective.refresh().is_err());
    std::fs::create_dir_all(root.join(".grapt/rules"))?;
    std::os::unix::fs::symlink(
        fixture.root.join("foreign.md"),
        root.join(".grapt/rules/link.md"),
    )?;
    let scoped = scoped_for_read(&root, &root.join("source.rs")).render_text();
    assert!(scoped.contains("resolves outside"));
    assert!(!scoped.contains("foreign policy"));
    Ok(())
}

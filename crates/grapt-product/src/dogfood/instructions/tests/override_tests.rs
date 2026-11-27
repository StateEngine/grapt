use super::*;

#[test]
fn claude_fallback_uses_the_same_ambient_and_scoped_selection(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let directory = fixture.root.join("project/sub");
    let fallback = directory.join("CLAUDE.md");
    std::fs::write(&fallback, "Claude project rule")?;
    assert!(text(&fixture.effective.refresh()?).contains("Claude project rule"));
    let scoped = scoped_for_read(&fixture.root.join("project"), &directory.join("file.rs"));
    assert!(scoped
        .instructions
        .iter()
        .any(|entry| entry.path == "sub/CLAUDE.md"));
    let agents = directory.join("AGENTS.md");
    std::fs::write(&agents, "")?;
    assert!(!text(&fixture.effective.refresh()?).contains("Claude project rule"));
    std::fs::write(&agents, [0xff])?;
    let (selected, content) = read_instruction_file(&directory, None);
    assert_eq!(selected, agents);
    assert!(content.is_err());
    std::fs::remove_file(agents)?;
    assert!(text(&fixture.effective.refresh()?).contains("Claude project rule"));
    Ok(())
}

#[test]
fn ambient_overrides_replace_only_their_directory_and_preserve_order(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    for (path, content) in [
        ("config/AGENTS.md", "global default"),
        ("config/AGENTS.override.md", "global override"),
        ("project/AGENTS.md", "project default"),
        ("project/sub/AGENTS.md", "near default"),
        ("project/sub/AGENTS.override.md", "near override"),
        ("AGENTS.override.md", "outside rule"),
    ] {
        std::fs::write(fixture.root.join(path), content)?;
    }
    let first = text(&fixture.effective.refresh()?);
    for expected in ["global override", "near override", "project default"] {
        assert!(
            first.contains(expected),
            "selected rule missing: {expected}"
        );
    }
    assert!(first.find("global override") < first.find("near override"));
    assert!(first.find("near override") < first.find("project default"));
    for missing in ["global default", "near default", "outside rule"] {
        assert!(!first.contains(missing), "unselected rule: {missing}");
    }
    assert!(first
        .replace('\\', "/")
        .contains("project/sub/AGENTS.override.md"));
    let history = fixture.history()?;
    // An unselected source cannot churn the provider prefix or instruction epoch.
    std::fs::write(
        fixture.root.join("project/sub/AGENTS.md"),
        "changed default",
    )?;
    assert_eq!(first, text(&fixture.effective.refresh()?));
    assert_eq!(history, fixture.history()?);
    std::fs::remove_file(fixture.root.join("project/sub/AGENTS.override.md"))?;
    let fallback = text(&fixture.effective.refresh()?);
    assert!(fallback.contains("changed default") && !fallback.contains("near override"));
    std::fs::write(fixture.root.join("project/sub/AGENTS.override.md"), "")?;
    let empty = text(&fixture.effective.refresh()?);
    assert!(!empty.contains("changed default"));
    assert!(empty
        .replace('\\', "/")
        .contains("project/sub/AGENTS.override.md"));
    Ok(())
}

#[test]
fn unusable_overrides_never_fall_back_to_the_default() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let directory = fixture.root.join("project/sub");
    std::fs::write(directory.join("AGENTS.md"), "fallback must stay hidden")?;
    let path = directory.join("AGENTS.override.md");
    std::fs::write(&path, "last good override")?;
    fixture.effective.refresh()?;
    for invalid in [vec![0xff], vec![b'x'; MAX_FILE_BYTES + 1]] {
        std::fs::write(&path, invalid)?;
        let rendered = text(&fixture.effective.refresh()?);
        assert!(rendered.contains("last good override"));
        assert!(!rendered.contains("fallback must stay hidden"));
        assert!(rendered.contains("temporarily unavailable"));
        let scoped = scoped_for_read(&fixture.root.join("project"), &directory.join("file.rs"));
        assert_eq!(scoped.instructions.len(), 1);
        assert_eq!(scoped.instructions[0].path, "sub/AGENTS.override.md");
        assert!(scoped.instructions[0].content.is_err());
    }
    std::fs::remove_file(&path)?;
    std::fs::create_dir(&path)?;
    let (selected, content) = read_instruction_file(&directory, None);
    assert_eq!(selected, path);
    assert!(content.is_err());
    Ok(())
}

#[test]
fn scoped_overrides_keep_nested_precedence_and_session_boundary(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let session = fixture.root.join("project/sub");
    std::fs::create_dir_all(session.join("a/b"))?;
    for (path, content) in [
        ("AGENTS.override.md", "ambient only"),
        ("a/AGENTS.md", "parent default"),
        ("a/AGENTS.override.md", "parent override"),
        ("a/b/AGENTS.md", "nested default"),
    ] {
        std::fs::write(session.join(path), content)?;
    }
    let scoped = scoped_for_read(&session, &session.join("a/b/file.rs"));
    let paths: Vec<&str> = scoped
        .instructions
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["a/b/AGENTS.md", "a/AGENTS.override.md"]);
    let rendered = scoped.render_text();
    assert!(rendered.contains("nested default") && rendered.contains("parent override"));
    assert!(rendered.find("nested default") < rendered.find("parent override"));
    assert!(!rendered.contains("parent default") && !rendered.contains("ambient only"));
    assert!(rendered.contains("user gives in this conversation override"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn linked_overrides_cannot_hide_unresolved_or_out_of_scope_sources(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new()?;
    let directory = fixture.root.join("project/sub");
    std::fs::write(directory.join("AGENTS.md"), "hidden fallback")?;
    let path = directory.join("AGENTS.override.md");
    std::os::unix::fs::symlink(fixture.root.join("missing"), &path)?;
    let (_, content) = read_instruction_file(&directory, None);
    assert!(content.is_err(), "a broken link was treated as absence");
    std::fs::remove_file(&path)?;
    std::fs::write(fixture.root.join("outside"), "outside override")?;
    std::os::unix::fs::symlink(fixture.root.join("outside"), &path)?;
    let ambient = fixture.effective.roots.ambient()?;
    assert!(ambient
        .failure
        .is_some_and(|reason| reason.contains("resolves outside")));
    let scoped = scoped_for_read(&directory, &directory.join("child/file.rs"));
    assert!(scoped.is_empty());
    let scoped = scoped_for_read(&fixture.root.join("project"), &directory.join("file.rs"));
    assert!(scoped.instructions[0]
        .content
        .as_ref()
        .is_err_and(|reason| reason.contains("resolves outside")));
    Ok(())
}

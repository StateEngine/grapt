use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn configured_and_markdown_agents_keep_project_limits_and_execution_modes() -> TestResult {
    let root = Root::new()?;
    let project = root.path().join("project");
    let config = root.path().join("config");
    let markdown = project.join(".grapt/agents/nested/helper.md");
    std::fs::create_dir_all(markdown.parent().ok_or("agent parent")?)?;
    std::fs::create_dir(&config)?;
    std::fs::write(
        config.join("grapt.json"),
        r#"{"agents":{"helper":{"system":"User instructions","tools":["read","edit"],"steps":5},"build":{"disabled":true}}}"#,
    )?;
    std::fs::write(
        project.join("grapt.json"),
        r#"{"agents":{"helper":{"steps":3},"build":{"disabled":false}}}"#,
    )?;
    std::fs::write(&markdown, "---\ndescription: Project helper\nmode: primary\nsteps: 8\ntools: [read, exec_command]\n---\nRead the relevant source.\n")?;
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(root.path()))?;
    let catalog = Catalog::from_sources(&sources)?;
    let helper = catalog
        .selected_for("helper", false)?
        .ok_or("helper missing")?;
    assert_eq!(helper.steps.map(std::num::NonZeroU32::get), Some(3));
    assert_eq!(
        helper.tools.as_ref(),
        Some(&BTreeSet::from(["read".to_owned()]))
    );
    assert!(helper
        .developer_instructions
        .contains("Read the relevant source."));
    assert!(catalog.selected_for("helper", true).is_err());
    assert!(catalog.selected("build").is_err());
    assert!(catalog.selected_for("plan", true).is_err());
    let inventory = catalog.inventory()?;
    assert!(!inventory.iter().any(|agent| agent["id"] == "build"));
    for agent in inventory {
        serde_json::from_value::<grapt_api::u3a::AgentInfoDto>(agent)?;
    }
    std::fs::write(&markdown, "---\nsteps: 0\n---\nInvalid step cap.\n")?;
    assert!(Catalog::from_sources(&sources).is_err());
    Ok(())
}

#[test]
fn project_definitions_replace_user_definitions_and_keep_catalog_order_stable() -> TestResult {
    let root = Root::new()?;
    let user = root.path().join("config/agents");
    let project = root.path().join("project/.grapt/agents");
    std::fs::create_dir_all(&user)?;
    std::fs::create_dir_all(&project)?;
    let definition = |prompt: &str| {
        format!("name = 'reviewer'\ndescription = 'Review changes'\ndeveloper_instructions = '{prompt}'\nmodel = 'fixture/fixture'\ntools = ['read']\n")
    };
    std::fs::write(user.join("review.toml"), definition("User review"))?;
    std::fs::write(
        project.join("review.toml"),
        definition("Project review").replace("['read']", "['read', 'exec_command']"),
    )?;
    let agents = root.catalog()?;
    let selected = agents.selected("reviewer")?.ok_or("missing reviewer")?;
    assert_eq!(selected.developer_instructions, "Project review");
    assert_eq!(
        selected.model()?,
        Some(json!({"providerID":"fixture","id":"fixture"}))
    );
    assert!(selected
        .tools
        .as_ref()
        .is_some_and(|tools| tools.contains("read") && !tools.contains("exec_command")));
    assert_eq!(
        agents.guidance("reviewer")?,
        root.catalog()?.guidance("reviewer")?
    );
    assert!(agents.selected("misspelled").is_err());
    Ok(())
}

#[test]
fn a_long_description_yields_reserved_space_to_its_diagnostics() -> TestResult {
    let root = Root::new()?;
    let claude_user = root.path().join("home/.claude/agents");
    std::fs::create_dir_all(&claude_user)?;
    let description = "d".repeat(1024);
    std::fs::write(
        claude_user.join("long.md"),
        format!("---\nname: long\ndescription: {description}\ntools: read, Bash\n---\nBODY\n"),
    )?;
    let agents = root.catalog()?;
    let inventory = agents.inventory()?;
    let row = inventory
        .iter()
        .find(|value| value["name"] == "long")
        .unwrap_or_else(|| panic!("missing long"));
    let text = row["description"]
        .as_str()
        .unwrap_or_else(|| panic!("description missing"));
    assert!(text.len() <= 1024, "row left the bound");
    assert!(
        text.contains("dropped unknown tools: Bash"),
        "the diagnostic disappeared: {text}"
    );
    assert!(text.contains('…'), "the truncation is not visible: {text}");
    assert!(
        !text.ends_with('…'),
        "the note must follow the marker: {text}"
    );
    Ok(())
}

#[test]
fn absent_claude_roots_stay_protected_and_join_the_catalog_roots() -> TestResult {
    let root = Root::new()?;
    let user = root.path().join("config/agents");
    let absent_user = root.path().join("home/.claude/agents");
    let absent_project = root.path().join("project/.claude/agents");
    std::fs::create_dir_all(&user)?;
    let agents = root.catalog()?;
    assert!(agents.roots.iter().any(|path| path == &user));
    for path in [&absent_user, &absent_project] {
        assert!(
            agents.roots.iter().any(|root| root == path),
            "{path:?} unprotected"
        );
    }
    Ok(())
}

#[test]
fn malformed_duplicate_and_oversized_agent_files_refuse_instead_of_disappearing() -> TestResult {
    let root = Root::new()?;
    let directory = root.path().join("config/agents");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("agent.toml");
    let valid = "name='worker'\ndescription='Task'\ndeveloper_instructions='Complete it'\n";
    for invalid in [
        format!("{valid}sandbox='bypass'\n"),
        valid.replace("Task", ""),
        "x".repeat(64 * 1024 + 1),
    ] {
        std::fs::write(&path, invalid)?;
        assert!(root.catalog().is_err());
    }
    std::fs::write(&path, valid)?;
    std::fs::write(directory.join("duplicate.toml"), valid)?;
    assert!(root.catalog().is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn definition_capture_refuses_links_and_fifos_substituted_after_discovery() -> TestResult {
    use std::os::unix::fs::symlink;
    let root = Root::new()?;
    let path = root.path().join("agent.toml");
    std::fs::write(
        &path,
        "name='worker'\ndescription='Work'\ndeveloper_instructions='Work'\n",
    )?;
    let paths = files::collect(root.path(), &mut 0, None, false, &mut Vec::new())?;
    std::fs::remove_file(&path)?;
    let target = root.path().join("replacement");
    std::fs::write(
        &target,
        "name='worker'\ndescription='Work'\ndeveloper_instructions='Work'\n",
    )?;
    symlink(&target, &path)?;
    assert!(files::read_toml(&paths[0], &mut 0).is_err());
    std::fs::remove_file(&path)?;
    let status = std::process::Command::new("mkfifo").arg(&path).status()?;
    assert!(status.success());
    assert!(files::read_toml(&paths[0], &mut 0).is_err());
    Ok(())
}

#[test]
fn claude_markdown_agents_load_below_our_definitions_and_report_dropped_tools() -> TestResult {
    let root = Root::new()?;
    let claude_user = root.path().join("home/.claude/agents");
    let claude_project = root.path().join("project/.claude/agents");
    let user = root.path().join("config/agents");
    std::fs::create_dir_all(&claude_user)?;
    std::fs::create_dir_all(&claude_project)?;
    std::fs::create_dir_all(&user)?;
    let markdown = |dir: &Path, file: &str, text: &str| -> Result<(), std::io::Error> {
        std::fs::write(dir.join(file), text)
    };
    markdown(
        &claude_user,
        "helper.md",
        "---\nname: helper\ndescription: Claude helper\nmodel: sonnet\ntools: read, Bash\n---\nUSER HELPER BODY\n",
    )?;
    markdown(
        &claude_project,
        "helper.md",
        "---\nname: helper\ndescription: Project Claude helper\nmodel: sonnet\ntools: [read, Grep]\n---\nPROJECT HELPER BODY\n",
    )?;
    // Same name as the built-in and as a user TOML: ours must win.
    markdown(
        &claude_project,
        "worker.md",
        "---\nname: worker\ndescription: Claude worker\n---\nCLAUDE WORKER BODY\n",
    )?;
    markdown(
        &claude_project,
        "reviewer.md",
        "---\nname: reviewer\ndescription: Claude reviewer\ntools: exec_command\n---\nCLAUDE REVIEWER BODY\n",
    )?;
    std::fs::write(
        user.join("reviewer.toml"),
        "name='reviewer'\ndescription='User reviewer'\ndeveloper_instructions='User review prompt'\n",
    )?;
    let agents = root.catalog()?;
    let inventory = agents.inventory()?;
    let row = |name: &str| -> &serde_json::Value {
        inventory
            .iter()
            .find(|value| value["name"] == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let helper = row("helper");
    assert_eq!(helper["source"], "claude-project");
    assert_eq!(helper["system"], "PROJECT HELPER BODY\n");
    assert_eq!(
        helper["description"],
        "Project Claude helper; dropped unknown tools: Grep"
    );
    assert!(
        helper.get("model").is_none(),
        "sonnet inherits, it does not route"
    );
    let worker = row("worker");
    assert_eq!(worker["source"], "builtin");
    assert_eq!(
        worker["system"],
        "Complete the assigned work; preserve unrelated edits by other workers."
    );
    let reviewer = row("reviewer");
    assert_eq!(reviewer["source"], "user");
    assert_eq!(reviewer["system"], "User review prompt");
    assert_eq!(reviewer["description"], "User reviewer");
    for name in ["default", "explorer"] {
        assert_eq!(row(name)["source"], "builtin");
    }
    let selected = agents.selected("helper")?.ok_or("helper missing")?;
    let tools = selected.tools.as_ref().ok_or("helper ceiling missing")?;
    assert_eq!(tools.len(), 1);
    assert!(tools.contains("read"));
    Ok(())
}

#[test]
fn native_formats_merge_fields_and_project_modes_only_narrow() -> TestResult {
    let root = Root::new()?;
    let project = root.path().join("project");
    let user = root.path().join("config");
    std::fs::create_dir_all(user.join("agents"))?;
    std::fs::create_dir_all(project.join(".grapt/agents"))?;
    std::fs::write(
        user.join("grapt.json"),
        r#"{"agents":{"build":{"mode":"primary","steps":3,"hidden":true,"color":"cyan","model":"fixture/model","system":"User prompt"}}}"#,
    )?;
    std::fs::write(
        user.join("agents/build.toml"),
        "description='Merged description'\n",
    )?;
    std::fs::write(
        project.join(".grapt/agents/build.md"),
        "---\nmode: all\nsteps: 9\n---\n",
    )?;
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(root.path()))?;
    let catalog = Catalog::from_sources(&sources)?;
    let agent = catalog.selected_for("build", false)?.ok_or("build")?;
    assert_eq!(agent.mode, Mode::Primary);
    assert_eq!(agent.steps.map(std::num::NonZeroU32::get), Some(3));
    assert!(agent.hidden);
    assert_eq!(agent.color.as_deref(), Some("cyan"));
    assert_eq!(agent.model.as_deref(), Some("fixture/model"));
    assert_eq!(agent.developer_instructions, "User prompt");
    assert_eq!(agent.description, "Merged description");
    assert!(catalog.selected_for("build", true).is_err());
    std::fs::write(
        project.join(".grapt/agents/build.md"),
        "---\nmode: subagent\n---\n",
    )?;
    let error = Catalog::from_sources(&sources)
        .err()
        .ok_or("mode conflict accepted")?;
    assert!(
        error.contains("build.md")
            && error.contains("build")
            && error.contains("conflicts with inherited primary")
            && error.contains("primary or all"),
        "{error}"
    );
    std::fs::write(
        project.join("grapt.json"),
        r#"{"agents":{"build":{"mode":"subagent"}}}"#,
    )?;
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(root.path()))?;
    let error = Catalog::from_sources(&sources)
        .err()
        .ok_or("configuration mode conflict accepted")?;
    assert!(
        error.contains("grapt.json") && error.contains("primary or all"),
        "{error}"
    );
    Ok(())
}

#[test]
fn native_source_errors_name_the_file_and_non_definitions_do_not_break_loading() -> TestResult {
    let root = Root::new()?;
    let project = root.path().join("project");
    let directory = project.join(".grapt/agents");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("README.md"), "# Agent documentation\n")?;
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(root.path()))?;
    Catalog::from_sources(&sources)?;
    let invalid = directory.join("helper.toml");
    std::fs::write(&invalid, "steps='wrong-type'\n")?;
    let error = Catalog::from_sources(&sources)
        .err()
        .ok_or("invalid agent accepted")?;
    assert!(
        error.contains("helper.toml") && error.contains("steps"),
        "{error}"
    );
    std::fs::remove_file(invalid)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("missing", directory.join("notes.txt"))?;
        Catalog::from_sources(&sources)?;
        std::os::unix::fs::symlink("missing", directory.join("helper.md"))?;
        let error = Catalog::from_sources(&sources)
            .err()
            .ok_or("linked definition accepted")?;
        assert!(
            error.contains("helper.md") && error.contains("link"),
            "{error}"
        );
    }
    Ok(())
}

#[test]
fn misnamed_definition_scopes_and_null_limits_refuse_with_a_cure() -> TestResult {
    let root = Root::new()?;
    let project = root.path().join("project");
    std::fs::create_dir_all(&project)?;
    for (raw, reason) in [
        (
            r#"{"agent":{"build":{"disabled":true}}}"#,
            "define roles under agents",
        ),
        (
            r#"{"agent":{"build":{"model":"fixture/model"}}}"#,
            "define roles under agents",
        ),
        (
            r#"{"agents":{"helper":{"steps":null}}}"#,
            "steps cannot be null",
        ),
        (
            r#"{"agents":{"helper":{"tools":null}}}"#,
            "tools cannot be null",
        ),
    ] {
        std::fs::write(project.join("grapt.json"), raw)?;
        let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(root.path()))?;
        let error = Catalog::from_sources(&sources)
            .err()
            .ok_or("bad configuration accepted")?;
        assert!(
            error.contains("grapt.json") && error.contains(reason),
            "{error}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn user_agent_directory_links_load_and_project_links_refuse() -> TestResult {
    let root = Root::new()?;
    let target = root.path().join("dotfiles");
    std::fs::create_dir(&target)?;
    std::fs::write(target.join("build.toml"), "steps=2\ntools=['read']\n")?;
    std::fs::write(
        target.join("helper.md"),
        "---\nname: helper\ndescription: Help\ntools: [read]\n---\nHelp from dotfiles.\n",
    )?;
    for relative in [
        "config/agent",
        "config/agents",
        "config/agents/nested",
        "home/.claude/agents",
    ] {
        let linked = root.path().join(relative);
        std::fs::create_dir_all(linked.parent().ok_or("link parent")?)?;
        std::os::unix::fs::symlink(&target, &linked)?;
        let catalog = root.catalog()?;
        let helper = catalog.selected("helper")?.ok_or("linked helper absent")?;
        assert!(helper
            .developer_instructions
            .contains("Help from dotfiles."));
        assert!(catalog.roots.contains(&std::fs::canonicalize(&target)?));
        if relative == "home/.claude/agents" {
            let home = root.path().join("home");
            let alias = root.path().join("home-alias");
            std::os::unix::fs::symlink(&home, &alias)?;
            for directory in [home, alias] {
                let sources =
                    CatalogSources::read(&directory, CatalogEnvironment::isolated(root.path()))?;
                let catalog = Catalog::from_sources(&sources)?;
                assert!(catalog.selected("helper")?.is_some());
                assert_eq!(claude::roots(&sources)?.len(), 1);
            }
        }
        if relative == "config/agent" || relative == "config/agents" {
            assert_eq!(
                catalog
                    .selected("build")?
                    .ok_or("build")?
                    .steps
                    .map(std::num::NonZeroU32::get),
                Some(2)
            );
        }
        std::fs::remove_file(linked)?;
    }
    for relative in [
        "project/.claude/agents",
        "project/.grapt/agents",
        "project/.grapt/agents/nested",
    ] {
        let linked = root.path().join(relative);
        std::fs::create_dir_all(linked.parent().ok_or("link parent")?)?;
        std::os::unix::fs::symlink(&target, &linked)?;
        let error = root
            .catalog()
            .err()
            .ok_or("linked agent source silently ignored")?;
        assert!(
            error.contains(linked.to_str().ok_or("link path")?)
                && error.contains("linked agent")
                && error.contains("regular"),
            "{error}"
        );
        std::fs::remove_file(linked)?;
    }
    Ok(())
}

struct Root(PathBuf);
impl Root {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        use grapt_types::IdAuthority;
        let ids = grapt_store::OsRandomIds::seed()?;
        let root = std::env::temp_dir().join(format!("agent-definitions-{}", ids.mint().as_str()));
        std::fs::create_dir(&root)?;
        Ok(Self(root))
    }
    fn catalog(&self) -> Result<Catalog, String> {
        let project = self.path().join("project");
        std::fs::create_dir_all(&project).map_err(|error| error.to_string())?;
        let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(self.path()))?;
        Catalog::from_sources(&sources)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

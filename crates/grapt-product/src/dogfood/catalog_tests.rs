use super::*;

#[test]
fn command_model_variant_is_separate_and_malformed_selection_is_refused() {
    assert_eq!(
        model_ref(&json!("anthropic/claude-opus-5#thinking")).ok(),
        Some(json!({"providerID":"anthropic","id":"claude-opus-5","variant":"thinking"}))
    );
    assert_eq!(
        model_ref(&json!("openai/group/model")).ok(),
        Some(json!({"providerID":"openai","id":"group/model"}))
    );
    for raw in [
        "/model",
        "provider/",
        "pro#vider/model",
        "provider/model#",
        "provider/model#a#b",
    ] {
        assert!(model_ref(&json!(raw)).is_err(), "accepted {raw}");
    }
}

fn write(path: &Path, bytes: &[u8]) {
    let parent = path
        .parent()
        .unwrap_or_else(|| panic!("fixture path has no parent"));
    std::fs::create_dir_all(parent).unwrap_or_else(|error| panic!("fixture directory: {error}"));
    std::fs::write(path, bytes).unwrap_or_else(|error| panic!("fixture file: {error}"));
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn retained_catalogs_fold_real_sources_and_never_hide_corruption_as_empty() {
    let root = std::env::temp_dir().join(format!("grapt-retained-catalogs-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("fixture reset: {error}"));
    }
    let project = root.join("project");
    let nested = project.join("nested");
    std::fs::create_dir_all(&nested).unwrap_or_else(|error| panic!("fixture project: {error}"));
    write(
        &root.join("config/grapt.json"),
        br#"{"commands":{"review":{"template":"global"},"context":{"template":"user context"},"fork":{"template":"user fork template"},"sandbox":{"template":"user sandbox template"}}}"#,
    );
    write(
        &root.join("config/command/review.md"),
        b"---\ndescription: global file\n---\nglobal markdown\n",
    );
    write(&project.join("grapt.json"), br#"{"commands":{"review":{"template":"project direct"},"context":{"template":"project context"},"fork":{"template":"project fork template"},"sandbox":{"template":"project sandbox template"}},"references":{"local-docs":{"path":"./docs","hidden":false}}}"#);
    write(&project.join(".grapt/grapt.jsonc"), br#"{
      // The supplementary document wins over the direct project document.
      "commands":{"review":{"template":"project supplementary"}},
      "references":{"effect":{"repository":"github.com/Effect-TS/effect-smol","branch":"review/base"}},
    }"#);
    write(
        &project.join(".grapt/command/review.md"),
        b"---\ndescription: project file\nmodel: openai/gpt-5.6\n---\nproject markdown\n",
    );
    write(&project.join(".grapt/skills/sample/SKILL.md"), b"---\nname: Sample skill\ndescription: |\n  Two lines\n  retained\nmetadata:\n  grapt/slash: 'false'\n  grapt/autoinvoke: true\n---\nSkill\tbody\n");
    let sources = CatalogSources::read(&nested, CatalogEnvironment::isolated(&root))
        .unwrap_or_else(|error| panic!("source capture: {error}"));
    let project_text = project
        .to_str()
        .unwrap_or_else(|| panic!("fixture path UTF-8"));
    let commands =
        commands(&sources, project_text).unwrap_or_else(|error| panic!("command fold: {error}"));
    assert!(commands
        .iter()
        .any(|row| row["name"] == "user:context" && row["template"] == "user context"));
    assert!(commands
        .iter()
        .any(|row| row["name"] == "project:context" && row["template"] == "project context"));
    assert!(!commands.iter().any(|row| row["name"] == "context"));
    for (name, template) in [
        ("user:fork", "user fork template"),
        ("project:fork", "project fork template"),
        ("user:sandbox", "user sandbox template"),
        ("project:sandbox", "project sandbox template"),
    ] {
        let fork = commands
            .iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("configured {name} is inaccessible behind the built-in"));
        assert_eq!(fork["template"], template);
    }
    assert!(!commands.iter().any(|row| row["name"] == "fork"));
    assert!(!commands.iter().any(|row| row["name"] == "sandbox"));
    let review = commands
        .iter()
        .find(|item| item["name"] == "project:review")
        .unwrap_or_else(|| panic!("review absent"));
    assert_eq!(
        review["template"], "project markdown",
        "catalog source precedence changed"
    );
    assert_eq!(review["description"], "project file");
    assert_eq!(
        review["model"],
        json!({"id":"gpt-5.6","providerID":"openai"})
    );
    let builtin = commands
        .iter()
        .find(|item| item["name"] == "review")
        .unwrap_or_else(|| panic!("built-in review absent"));
    assert_ne!(
        builtin["template"], review["template"],
        "repository shadowed built-in review"
    );
    let user = commands
        .iter()
        .find(|item| item["name"] == "user:review")
        .unwrap_or_else(|| panic!("user collision absent"));
    assert_eq!(user["template"], "global markdown");
    let init = commands
        .iter()
        .find(|item| item["name"] == "init")
        .unwrap_or_else(|| panic!("init absent"));
    assert!(init["template"]
        .as_str()
        .is_some_and(|text| text.contains(project_text)));
    let skills = skills(&sources, &nested).unwrap_or_else(|error| panic!("skill fold: {error}"));
    // The scratch lease may place this temp root below the machine's home, and
    // CatalogSources::read walks every ancestor for `.claude`/`.agents` skill
    // sources, so the machine's own global skills (a user-global
    // `.claude/skills` under the home that contains the lease) legitimately
    // fold in. Measure that contribution through the same code path on an
    // empty sibling fixture, then assert the fixture contributes exactly one
    // skill of its own.
    let bare = root.join("bare");
    let bare_nested = bare.join("project").join("nested");
    std::fs::create_dir_all(&bare_nested).unwrap_or_else(|error| panic!("bare fixture: {error}"));
    let bare_sources = CatalogSources::read(&bare_nested, CatalogEnvironment::isolated(&bare))
        .unwrap_or_else(|error| panic!("bare source capture: {error}"));
    let baseline = super::skills(&bare_sources, &bare_nested)
        .unwrap_or_else(|error| panic!("baseline fold: {error}"));
    assert_eq!(
        skills.len(),
        baseline.len() + 1,
        "fixture skill is not exactly one entry beyond the machine's own sources"
    );
    let sample = skills
        .iter()
        .find(|item| item["id"] == "sample")
        .unwrap_or_else(|| panic!("sample skill absent"));
    assert_eq!(sample["id"], "sample");
    assert_eq!(sample["content"], "Skill\tbody\n");
    assert_eq!(sample["slash"], false);
    assert_eq!(sample["autoinvoke"], true);
    let references = references(&sources).unwrap_or_else(|error| panic!("reference fold: {error}"));
    assert_eq!(references.len(), 2);
    let local = references
        .iter()
        .find(|item| item["name"] == "local-docs")
        .unwrap_or_else(|| panic!("local reference absent"));
    assert_eq!(
        Path::new(
            local["path"]
                .as_str()
                .unwrap_or_else(|| panic!("local path absent"))
        ),
        project.join("./docs")
    );
    let remote = references
        .iter()
        .find(|item| item["name"] == "effect")
        .unwrap_or_else(|| panic!("remote reference absent"));
    assert_eq!(remote["source"]["branch"], "review/base");
    assert!(remote["path"]
        .as_str()
        .is_some_and(|path| path.ends_with("effect-smol@review%2Fbase")));
    // Preserve typed wire shapes as well as the projection values.
    let location =
        json!({"directory":project_text,"project":{"id":"prj_catalog","directory":project_text}});
    for result in [
        serde_json::from_value::<grapt_api::u3a::CommandListResponseDto>(
            json!({"location":location,"data":commands}),
        )
        .map(|_| ()),
        serde_json::from_value::<grapt_api::u3a::SkillListResponseDto>(
            json!({"location":location,"data":skills}),
        )
        .map(|_| ()),
        serde_json::from_value::<grapt_api::u3a::ReferenceListResponseDto>(
            json!({"location":location,"data":references}),
        )
        .map(|_| ()),
    ] {
        result.unwrap_or_else(|error| panic!("catalog DTO rejected real projection: {error}"));
    }
    write(&project.join(".grapt/grapt.jsonc"), b"{\"commands\":");
    assert!(
        CatalogSources::read(&nested, CatalogEnvironment::isolated(&root)).is_err(),
        "corrupt configuration became an empty catalog"
    );
    std::fs::remove_file(project.join(".grapt/grapt.jsonc"))
        .unwrap_or_else(|error| panic!("remove corrupt fixture: {error}"));
    let repaired = CatalogSources::read(&nested, CatalogEnvironment::isolated(&root))
        .unwrap_or_else(|error| {
            panic!("absent source was not distinguished from corrupt: {error}")
        });
    assert!(!super::commands(&repaired, project_text)
        .unwrap_or_else(|error| panic!("repaired commands: {error}"))
        .is_empty());
    std::fs::remove_dir_all(root).unwrap_or_else(|error| panic!("fixture cleanup: {error}"));
}

#[test]
fn reference_cache_paths_refuse_escape_and_catalogs_refuse_overflow() {
    let root = Path::new(if cfg!(windows) {
        "E:/catalog-cache"
    } else {
        "/catalog-cache"
    });
    assert!(repository_path(root, "github.com/org/../escape", None).is_err());
    assert!(repository_path(root, "github.com/org/repo", Some("../../escape")).is_err());
    let entries = (0..257)
        .map(|index| (index.to_string(), json!({"name":index})))
        .collect();
    assert!(check_catalog(&entries).is_err());
}

#[test]
fn skd_offered_skills_need_a_description_and_respect_autoinvoke() {
    let skills = vec![
        json!({"id":"zeta","name":"Ada","description":"last","content":"z"}),
        json!({"id":"alpha","description":"first","content":"a"}),
        json!({"id":"manual","description":"manual","autoinvoke":false,"content":"m"}),
        json!({"id":"blank","description":"   ","content":"b"}),
        json!({"id":"bare","content":"c"}),
    ];
    let offered = super::offered(&skills);
    let ids: Vec<&str> = offered.iter().map(|skill| skill.id.as_str()).collect();
    assert_eq!(ids, vec!["alpha", "zeta"]);
    assert_eq!(
        offered[0].name, "alpha",
        "the id names a skill without a name"
    );
    assert_eq!(offered[1].name, "Ada");
}

#[test]
fn file_skills_join_commands_with_standard_invocation_flags_and_provenance(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-skill-commands-{}", std::process::id()));
    assert!(!root.exists(), "fixture already exists");
    let project = root.join("project");
    let directory = project.join("sub");
    write(
        &project.join("grapt.json"),
        br#"{"skills":["https://user:synthetic-secret@skills.invalid/catalog?token=synthetic-secret","./local-skills","https://other.invalid/skills"],"commands":{"collision":{"template":"custom"}}}"#,
    );
    let file = directory.join("local-skills/draft-release/SKILL.md");
    write(&file, b"---\nname: Draft release\ndescription: Prepare a release\nargument-hint: topic\nslash: false\nuser-invocable: true\ndisable-model-invocation: true\n---\nWrite $ARGUMENTS.\n");
    write(&directory.join("local-skills/private-helper/SKILL.md"), b"---\ndescription: Model helper\nuser-invocable: false\n---\nHidden from slash commands.\n");
    write(
        &directory.join("local-skills/collision/SKILL.md"),
        b"---\ndescription: Skill collision\n---\nSkill must not replace custom.\n",
    );
    write(
        &directory.join("local-skills/hint-only/SKILL.md"),
        b"---\nargument-hint: topic\n---\nNo model description.\n",
    );
    write(
        &directory.join("local-skills/blank-description/SKILL.md"),
        b"---\ndescription: '  '\nargument-hint: topic\n---\nBlank model description.\n",
    );
    let sources = CatalogSources::read(&directory, CatalogEnvironment::isolated(&root))?;
    let rows = command::commands(&sources, project.to_str().ok_or("project UTF-8")?)?;
    let release = rows
        .iter()
        .find(|row| row["name"] == "draft-release")
        .ok_or("skill command absent")?;
    assert_eq!(release["_origin"], "project");
    assert_eq!(
        std::fs::canonicalize(release["_source"].as_str().ok_or("source path")?)?,
        std::fs::canonicalize(&file)?
    );
    assert_eq!(release["description"], "Prepare a release topic");
    assert!(release["template"]
        .as_str()
        .is_some_and(|text| text.contains("Write $ARGUMENTS.")));
    assert!(!rows.iter().any(|row| row["name"] == "private-helper"));
    assert!(rows
        .iter()
        .any(|row| row["name"] == "collision" && row["template"] == "custom"));
    serde_json::from_value::<grapt_api::u3a::CommandInfoDto>(command::strip_source(
        release.clone(),
    ))?;
    let loaded = skills(&sources, &directory)?;
    let warnings = skill_source_warnings(&skill_sources(&sources, &directory)?)?;
    assert_eq!(warnings.len(), 2);
    assert!(warnings.iter().all(
        |warning| warning.contains("local directory") && !warning.contains("synthetic-secret")
    ));
    assert!(warnings
        .iter()
        .any(|warning| warning.contains("https://skills.invalid")));
    assert!(warnings
        .iter()
        .any(|warning| warning.contains("https://other.invalid")));
    for skill in &loaded {
        serde_json::from_value::<grapt_api::u3a::SkillInfoDto>(skill.clone())?;
    }
    assert!(offered(&loaded)
        .iter()
        .any(|skill| skill.id == "private-helper"));
    assert!(!offered(&loaded).iter().any(|skill| matches!(
        skill.id.as_str(),
        "draft-release" | "hint-only" | "blank-description"
    )));
    write(&file, b"---\nuser-invocable: maybe\n---\nBad flag.\n");
    assert!(skills(&sources, &directory).is_err());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

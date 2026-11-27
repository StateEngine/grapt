//! Exercise installation as one operation, including the publication failure seam.
use super::*;
use grapt_api::u3a::ClaudePluginRequestDto;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    environment: CatalogEnvironment,
    workspace: PathBuf,
    market: PathBuf,
}
impl Fixture {
    fn new() -> Result<Self, String> {
        let root = std::env::temp_dir().join(format!(
            "secode-plugin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let environment = CatalogEnvironment::isolated(&root);
        destination::Parent::open(&environment.config.join(".fixture-probe"), true).map_err(io)?;
        let workspace = root.join("work $1 ${CLAUDE_PLUGIN_ROOT}");
        let market = root.join("market");
        for path in [
            &workspace,
            &environment.home,
            &environment.service_root().to_path_buf(),
            &market,
        ] {
            std::fs::create_dir_all(path).map_err(io)?;
        }
        write(&market.join(".claude-plugin/marketplace.json"),&json!({"name":"local","owner":{"name":"test"},"plugins":[{"name":"one","source":"./one"},{"name":"two","source":"./two"}]}).to_string())?;
        for name in ["one", "two"] {
            write(
                &market.join(name).join(".claude-plugin/plugin.json"),
                &json!({"name":name}).to_string(),
            )?;
            write(&market.join(name).join("skills/review/SKILL.md"),"---\ndescription: Review changes\n---\nRoot ${CLAUDE_PLUGIN_ROOT}; data ${CLAUDE_PLUGIN_DATA}; project ${CLAUDE_PROJECT_DIR}; $ARGUMENTS")?;
            write(
                &market.join(name).join("commands/hello.md"),
                "---\ndescription: Say hello\n---\nHello $ARGUMENTS from ${CLAUDE_PLUGIN_ROOT}",
            )?;
        }
        Ok(Self {
            root,
            environment,
            workspace,
            market,
        })
    }
    fn operation(&self, value: Value) -> Result<String, String> {
        let request = serde_json::from_value(value).map_err(|_| "fixture request is invalid")?;
        operation::perform_with(
            self.environment.clone(),
            self.workspace.to_str().ok_or("fixture path is not UTF-8")?,
            request,
        )
        .map(|r| r.message.as_str().to_owned())
    }
    fn add(&self) -> Result<(), String> {
        self.operation(json!({"action":"marketplace-add","source":self.market}))?;
        Ok(())
    }
    fn sources(&self) -> Result<CatalogSources, String> {
        CatalogSources::read(&self.workspace, self.environment.clone())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::create_dir_all(path.parent().ok_or("fixture path has no parent")?).map_err(io)?;
    std::fs::write(path, text).map_err(io)
}

#[test]
fn local_install_is_idempotent_and_concurrent_requests_keep_one_complete_tree() -> Result<(), String>
{
    let fixture = Fixture::new()?;
    write(
        &fixture.market.join("one/commands/hidden.md"),
        "---\nuser-invocable: false\n---\nHidden command",
    )?;
    write(
        &fixture.market.join("one/commands/internal.md"),
        "---\nmetadata:\n  grapt/slash: false\n---\nInternal command",
    )?;
    fixture.add()?;
    let mut results = Vec::new();
    std::thread::scope(|scope| {
        let left =
            scope.spawn(|| fixture.operation(json!({"action":"install","plugin":"one@local"})));
        let right =
            scope.spawn(|| fixture.operation(json!({"action":"install","plugin":"one@local"})));
        results.push(left.join().map_err(|_| "installer thread panicked")?);
        results.push(right.join().map_err(|_| "installer thread panicked")?);
        Ok::<_, String>(())
    })?;
    assert!(results.iter().any(Result::is_ok));
    assert!(fixture
        .operation(json!({"action":"install","plugin":"one@local"}))?
        .contains("already installed"));
    let sources = fixture.sources()?;
    let skills = skills(&sources)?;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0]["skill"]["id"], "one:review");
    let body = skills[0]["skill"]["content"]
        .as_str()
        .ok_or("skill has no body")?;
    assert!(body.contains("work $1 ${CLAUDE_PLUGIN_ROOT}"));
    assert!(body.contains("claude-plugins/data/"));
    let activated = components::activate_skill_from_sources(&sources, "one:review", "chosen $0")?
        .ok_or("plugin skill activation missing")?;
    assert!(activated.ends_with("chosen $0"));
    assert!(activated.contains("work $1 ${CLAUDE_PLUGIN_ROOT}"));
    let commands = commands(&sources)?;
    assert!(commands
        .iter()
        .any(|command| command["name"] == "one:hello"));
    assert!(!commands.iter().any(|command| matches!(
        command["name"].as_str(),
        Some("one:hidden" | "one:internal")
    )));
    assert_eq!(
        std::fs::read_dir(fixture.environment.config.join("claude-plugins/cache"))
            .map_err(io)?
            .count(),
        1
    );
    Ok(())
}

#[test]
fn unusable_command_names_and_metadata_fail_before_activation() -> Result<(), String> {
    let fixture = Fixture::new()?;
    fixture.add()?;
    let nested = std::iter::repeat_n("x".repeat(100), 6)
        .collect::<Vec<_>>()
        .join("/");
    for (name, body) in [
        ("not invocable".to_owned(), "Ordinary prompt".to_owned()),
        (nested, "Ordinary prompt".to_owned()),
        (
            "metadata".to_owned(),
            format!(
                "---\ndescription: {}\nargument-hint: {}\n---\nOrdinary prompt",
                "d".repeat(5000),
                "h".repeat(5000)
            ),
        ),
    ] {
        let path = fixture
            .market
            .join("one/commands")
            .join(format!("{name}.md"));
        write(&path, &body)?;
        let error = fixture
            .operation(json!({"action":"install","plugin":"one@local"}))
            .err()
            .ok_or("unusable command catalog activated")?;
        assert!(error.contains("command catalog contract"), "{error}");
        assert!(loaded(&fixture.sources()?)?.is_empty());
        std::fs::remove_file(path).map_err(io)?;
    }
    Ok(())
}

#[test]
fn failed_config_publication_retains_unbound_and_live_trees_without_activation(
) -> Result<(), String> {
    let fixture = Fixture::new()?;
    fixture.add()?;
    fixture.operation(json!({"action":"install","plugin":"one@local"}))?;
    let live = loaded(&fixture.sources()?)?;
    let old = live
        .first()
        .ok_or("active install is missing")?
        .installation
        .root
        .clone();
    let config = fixture.environment.config.join("grapt.jsonc");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&config).map_err(io)?)
        .map_err(|_| "config is invalid")?;
    let length = serde_json::to_vec(&value)
        .map_err(|_| "fixture encoding failed")?
        .len();
    value["padding"] = json!("x".repeat(65_500usize.saturating_sub(length + 14)));
    let before = serde_json::to_vec(&value).map_err(|_| "fixture encoding failed")?;
    std::fs::write(&config, &before).map_err(io)?;
    let error = fixture
        .operation(json!({"action":"install","plugin":"two@local"}))
        .err()
        .ok_or("oversized publication succeeded")?;
    assert!(error.contains("configuration bound"), "{error}");
    assert_eq!(std::fs::read(&config).map_err(io)?, before);
    assert!(old.join("skills/review/SKILL.md").is_file());
    assert_eq!(Snapshot::capture(&old)?.digest, live[0].snapshot.digest);
    assert_eq!(loaded(&fixture.sources()?)?.len(), 1);
    assert_eq!(
        std::fs::read_dir(fixture.environment.config.join("claude-plugins/cache"))
            .map_err(io)?
            .count(),
        2
    );
    Ok(())
}

#[test]
fn unsupported_implicit_components_and_untrusted_registry_never_activate() -> Result<(), String> {
    let fixture = Fixture::new()?;
    fixture.add()?;
    for name in ["output-styles", "themes", "monitors", "bin", "workflows"] {
        let directory = fixture.market.join("one").join(name);
        std::fs::create_dir(&directory).map_err(io)?;
        let error = fixture
            .operation(json!({"action":"install","plugin":"one@local"}))
            .err()
            .ok_or("implicit component activated")?;
        assert!(error.contains("not supported"), "{error}");
        std::fs::remove_dir(directory).map_err(io)?;
    }
    for name in ["settings.json", ".mcp.json", ".lsp.json"] {
        let path = fixture.market.join("one").join(name);
        write(&path, "{}")?;
        assert!(fixture
            .operation(json!({"action":"install","plugin":"one@local"}))
            .is_err());
        std::fs::remove_file(path).map_err(io)?;
    }
    assert!(loaded(&fixture.sources()?)?.is_empty());
    write(
        &fixture.workspace.join("grapt.json"),
        r#"{"claudePlugins":{"installed":{}}}"#,
    )?;
    assert!(loaded(&fixture.sources()?).is_err());
    let _request: ClaudePluginRequestDto =
        serde_json::from_value(json!({"action":"install","plugin":"one@local"}))
            .map_err(|_| "request invalid")?;
    Ok(())
}

#[test]
fn changed_installed_content_and_source_links_refuse_activation() -> Result<(), String> {
    let fixture = Fixture::new()?;
    fixture.add()?;
    let link = fixture.market.join("one/linked.md");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(fixture.market.join("two/commands/hello.md"), &link)
            .map_err(io)?;
        assert!(fixture
            .operation(json!({"action":"install","plugin":"one@local"}))
            .is_err());
        std::fs::remove_file(&link).map_err(io)?;
    }
    std::fs::hard_link(fixture.market.join("two/commands/hello.md"), &link).map_err(io)?;
    assert!(fixture
        .operation(json!({"action":"install","plugin":"one@local"}))
        .is_err());
    std::fs::remove_file(link).map_err(io)?;
    fixture.operation(json!({"action":"install","plugin":"one@local"}))?;
    let loaded = loaded(&fixture.sources()?)?;
    let path = loaded[0].installation.root.join("commands/hello.md");
    let mut permissions = std::fs::metadata(&path).map_err(io)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o600);
    }
    #[cfg(windows)]
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows-only tamper fixture; Unix uses an explicit private mode above"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).map_err(io)?;
    std::fs::write(path, "changed").map_err(io)?;
    assert!(super::loaded(&fixture.sources()?).is_err());
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn remote_catalog_pins_relative_installs_and_refuses_changed_provenance() -> Result<(), String> {
    let fixture = Fixture::new()?;
    let directory = fixture
        .workspace
        .to_str()
        .ok_or("fixture path is not UTF-8")?;
    let catalog_root = fixture.root.join("catalog-only");
    write(
        &catalog_root.join(".claude-plugin/marketplace.json"),
        &std::fs::read_to_string(fixture.market.join(".claude-plugin/marketplace.json"))
            .map_err(io)?,
    )?;
    let source = grapt_plugin::claude::GitSource::marketplace("team/plugins#release")?;
    let commit = "ab".repeat(20);
    let request = || {
        serde_json::from_value::<ClaudePluginRequestDto>(json!({
            "action":"marketplace-add", "source":"team/plugins#release"
        }))
        .map_err(|_| "fixture request is invalid".to_owned())
    };
    for _ in 0..2 {
        operation::publish_with(
            fixture.environment.clone(),
            directory,
            request()?,
            Some(remote::Prepared {
                kind: remote::Kind::Marketplace(source.clone()),
                acquired: acquisition::Acquired {
                    snapshot: Snapshot::capture(&catalog_root)?,
                    commit: commit.clone(),
                },
            }),
        )?;
    }
    let read_registry = || {
        let bytes = std::fs::read(fixture.environment.config.join("grapt.jsonc")).map_err(io)?;
        operation::decode_registry(&bytes)
    };
    let market_name = Name::new("local")?;
    let market = read_registry()?
        .marketplaces
        .get(&market_name)
        .ok_or("marketplace is missing")?
        .clone();
    assert_eq!(
        market
            .remote
            .as_ref()
            .ok_or("remote provenance is absent")?
            .commit,
        commit
    );
    assert_eq!(
        std::fs::read_dir(fixture.environment.config.join("claude-plugins/cache"))
            .map_err(io)?
            .count(),
        1
    );
    let install = |name: &str| {
        serde_json::from_value::<ClaudePluginRequestDto>(json!({
            "action":"install", "plugin":format!("{name}@local")
        }))
        .map_err(|_| "fixture install request is invalid".to_owned())
    };
    let prepared = remote::preflight(fixture.environment.clone(), directory, &install("one")?);
    if cfg!(any(windows, target_os = "linux", target_os = "android")) {
        assert!(prepared?.is_some());
    } else {
        assert!(prepared.is_err());
    }
    // Preflight owns no lock while acquisition runs.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.environment.config.join(".mcp-config.lock"))
        .map_err(io)?;
    lock.try_lock()
        .map_err(|_| "remote preflight retained the user configuration lock")?;
    drop(lock);
    operation::publish_with(
        fixture.environment.clone(),
        directory,
        install("one")?,
        Some(remote::Prepared {
            kind: remote::Kind::Plugin(market.clone()),
            acquired: acquisition::Acquired {
                snapshot: Snapshot::capture(&fixture.market.join("one"))?,
                commit: commit.clone(),
            },
        }),
    )?;
    assert!(commands(&fixture.sources()?)?
        .iter()
        .any(|command| command["name"] == "one:hello"));
    assert!(remote::preflight(fixture.environment.clone(), directory, &install("one")?)?.is_none());
    super::super::mcp_config::file::publish(fixture.environment.clone(), |before, _, _| {
        let mut document: Value =
            serde_json::from_slice(before).map_err(|_| "fixture config is invalid")?;
        document["claudePlugins"]["marketplaces"]["local"]["remote"]["commit"] =
            Value::String("cd".repeat(20));
        Ok((
            serde_json::to_vec(&document).map_err(|_| "fixture config cannot encode")?,
            (),
        ))
    })?;
    let refusal = operation::publish_with(
        fixture.environment.clone(),
        directory,
        install("two")?,
        Some(remote::Prepared {
            kind: remote::Kind::Plugin(market),
            acquired: acquisition::Acquired {
                snapshot: Snapshot::capture(&fixture.market.join("two"))?,
                commit,
            },
        }),
    )
    .err()
    .ok_or("changed remote provenance activated")?;
    assert!(refusal.contains("provenance changed"), "{refusal}");
    assert!(!read_registry()?
        .installed
        .contains_key(&PluginId::new("two@local")?));
    let credential_source = serde_json::from_value::<ClaudePluginRequestDto>(json!({
        "action":"marketplace-add", "source":"https://user:private-secret@example.test/plugins"
    }))
    .map_err(|_| "fixture credential request is invalid")?;
    let refusal = remote::preflight(fixture.environment.clone(), directory, &credential_source)
        .err()
        .ok_or("embedded remote credentials were admitted")?;
    assert!(
        refusal.contains(
            if cfg!(any(windows, target_os = "linux", target_os = "android")) {
                "without embedded credentials"
            } else {
                "not supported on this platform"
            }
        ),
        "{refusal}"
    );
    assert!(!refusal.contains("private-secret"));
    Ok(())
}

#[test]
fn installed_agents_keep_namespace_tool_ceilings_and_declared_selection() -> Result<(), String> {
    use super::super::agent_definitions::Catalog;
    let fixture = Fixture::new()?;
    write(
        &fixture.environment.config.join("grapt.jsonc"),
        &json!({"agents":{"modelAliases":{"haiku":"fixture/small"}}}).to_string(),
    )?;
    fixture.add()?;
    write(&fixture.market.join("one/agents/reviewer.md"), "---\nname: reviewer\ndescription: Review safely\ntools: Read, Write, Bash, Grep\ndisallowedTools: [Write, Edit]\nmodel: haiku\ncolor: cyan\npermissionMode: bypassPermissions\nhooks: {}\nmcpServers: []\n---\nAgent root ${CLAUDE_PLUGIN_ROOT}; project ${CLAUDE_PROJECT_DIR}\n")?;
    write(
        &fixture.market.join("one/agents/nested/check.md"),
        "---\ndescription: Nested helper\n---\nHelp with the assigned task.\n",
    )?;
    fixture.operation(json!({"action":"install","plugin":"one@local"}))?;
    let sources = fixture.sources()?;
    let agents = Catalog::from_sources(&sources)?;
    assert!(agents.selected("one:nested:check")?.is_some());
    let role = agents
        .selected("one:reviewer")?
        .ok_or("installed agent missing")?;
    let tools = role.tools.as_ref().ok_or("agent tool ceiling missing")?;
    assert!(tools.contains("read") && tools.contains("grep") && tools.contains("exec_command"));
    assert!(!tools.contains("write") && !tools.contains("edit") && !tools.contains("patch"));
    assert_eq!(
        role.model()?,
        Some(json!({"providerID":"fixture","id":"small"}))
    );
    let installation = loaded(&sources)?
        .pop()
        .ok_or("plugin missing")?
        .installation;
    assert!(agents.roots.contains(&installation.root));
    assert_eq!(
        role.developer_instructions.trim(),
        format!(
            "Agent root {}; project {}",
            installation.root.to_string_lossy().replace('\\', "/"),
            fixture.workspace.to_string_lossy().replace('\\', "/")
        )
    );
    assert!(agents
        .inventory()?
        .iter()
        .any(|agent| agent["id"] == "one:reviewer"
            && agent["source"] == "claude-plugin"
            && agent["color"] == "cyan"));
    // The normal project layer can replace prompt metadata but cannot widen
    // the admitted plugin role's tool ceiling.
    write(&fixture.workspace.join(".grapt/agents/override.toml"), "name='one:reviewer'\ndescription='Project review'\ndeveloper_instructions='Project instructions'\ntools=['read','write']\n")?;
    let narrowed = Catalog::from_sources(&fixture.sources()?)?;
    let role = narrowed
        .selected("one:reviewer")?
        .ok_or("project agent missing")?;
    assert_eq!(role.developer_instructions, "Project instructions");
    assert_eq!(
        role.tools.as_ref(),
        Some(&std::collections::BTreeSet::from(["read".to_owned()]))
    );
    for declaration in [
        "memory: user",
        "maxTurns: 3",
        "unknownField: true",
        "tools: [UnavailableTool]",
        "disallowedTools: [UnavailableTool]",
    ] {
        write(
            &fixture.market.join("two/agents/unsupported.md"),
            &format!(
                "---\nname: unsupported\ndescription: Unsupported\n{declaration}\n---\nBody\n"
            ),
        )?;
        let refusal = fixture
            .operation(json!({"action":"install","plugin":"two@local"}))
            .err()
            .ok_or("unsupported agent activated")?;
        assert!(refusal.contains("not supported"), "{refusal}");
        assert_eq!(loaded(&fixture.sources()?)?.len(), 1);
    }
    // Explicit agent files replace the default scan; unrelated default files
    // remain resources and do not acquire execution semantics.
    write(
        &fixture.market.join("two/.claude-plugin/plugin.json"),
        &json!({"name":"two","agents":["./special/helper.md"]}).to_string(),
    )?;
    write(
        &fixture.market.join("two/special/helper.md"),
        "---\nname: helper\ndescription: Help\n---\nHelp with the assigned task.\n",
    )?;
    fixture.operation(json!({"action":"install","plugin":"two@local"}))?;
    let agents = Catalog::from_sources(&fixture.sources()?)?;
    assert!(agents.selected("two:helper")?.is_some());
    assert!(agents.selected("two:unsupported").is_err());
    Ok(())
}

#[tokio::test]
async fn remote_refs_cannot_be_fetch_refspecs() -> Result<(), String> {
    let fixture = Fixture::new()?;
    let directory = fixture
        .workspace
        .to_str()
        .ok_or("fixture path is not UTF-8")?;
    let request = serde_json::from_value::<ClaudePluginRequestDto>(json!({
        "action":"marketplace-add",
        "source":"https://127.0.0.1:1/plugins#HEAD:refs/heads/other"
    }))
    .map_err(|_| "fixture remote request is invalid")?;
    if !cfg!(any(windows, target_os = "linux", target_os = "android")) {
        let refusal = remote::preflight(fixture.environment.clone(), directory, &request)
            .err()
            .ok_or("unavailable directory anchoring was admitted")?;
        assert!(
            refusal.contains("not supported on this platform"),
            "{refusal}"
        );
        assert!(loaded(&fixture.sources()?)?.is_empty());
        return Ok(());
    }
    let plan = remote::preflight(fixture.environment.clone(), directory, &request)?
        .ok_or("remote source did not produce a plan")?;
    let refusal = plan
        .acquire()
        .await
        .err()
        .ok_or("fetch refspec was admitted")?;
    assert!(refusal.contains("literal branch or tag"), "{refusal}");
    assert!(loaded(&fixture.sources()?)?.is_empty());
    let retained = std::fs::read_dir(
        fixture
            .environment
            .config
            .join("claude-plugins/acquisition"),
    )
    .map_err(io)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(io)?;
    assert!(
        retained.is_empty(),
        "ordinary ref refusal retained acquisition scratch"
    );
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let root = acquisition::scratch(&fixture.environment.config)?;
        let held = super::acquisition_guard::Guard::open(&root).map_err(io)?;
        let moved = root.with_extension("retained");
        let outside = fixture.root.join("outside");
        std::fs::create_dir(&outside).map_err(io)?;
        write(&outside.join("keep"), "outside bytes must survive")?;
        std::fs::rename(&root, &moved).map_err(io)?;
        std::os::unix::fs::symlink(&outside, &root).map_err(io)?;
        super::git_command::run(
            std::ffi::OsStr::new("git"),
            held.path(),
            &["init", "--bare", "--quiet"],
            4096,
        )
        .await
        .map_err(|error| error.message)?;
        assert!(moved.join("config").is_file());
        assert!(!outside.join("config").exists());
        assert!(held.cleanup(&root).is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("keep")).map_err(io)?,
            "outside bytes must survive"
        );
        std::fs::remove_file(&root).map_err(io)?;
        std::fs::rename(&moved, &root).map_err(io)?;
        super::acquisition_guard::Guard::open(&root)
            .map_err(io)?
            .cleanup(&root)
            .map_err(io)?;
        assert!(!root.exists());
    }
    Ok(())
}

#[test]
fn installed_hook_sources_merge_and_refuse_unsupported_execution_before_activation(
) -> Result<(), String> {
    use crate::dogfood::hooks::{Event, HookSettings};
    let fixture = Fixture::new()?;
    let group = json!([{"hooks":[{"type":"command","command":"echo plugin"}]}]);
    write(
        &fixture.market.join("one/hooks/hooks.json"),
        &json!({"description":"original hook wrapper","hooks":{"SessionStart":group}}).to_string(),
    )?;
    write(
        &fixture.market.join("one/extra.json"),
        &json!({"hooks":{"Stop":group}}).to_string(),
    )?;
    write(
        &fixture.market.join("one/.claude-plugin/plugin.json"),
        &json!({"name":"one","hooks":["./extra.json",{"SessionEnd":group}]}).to_string(),
    )?;
    let program = std::env::current_exe().map_err(io)?;
    write(
        &fixture.environment.config.join("grapt.json"),
        &json!({"hook_commands":{
        "user-first":{"command":[program]}, "project-last":{"command":[program]}
    },"hooks":{"SessionStart":["user-first"]}})
        .to_string(),
    )?;
    write(
        &fixture.workspace.join("grapt.json"),
        &json!({"hooks":{"SessionStart":["project-last"]}}).to_string(),
    )?;
    fixture.add()?;
    fixture.operation(json!({"action":"install","plugin":"one@local"}))?;
    let sources = fixture.sources()?;
    let settings = HookSettings::from_catalog(&sources)?;
    for event in [Event::SessionStart, Event::Stop, Event::SessionEnd] {
        let command = settings
            .commands(event)
            .iter()
            .find(|command| command.name.as_str().starts_with("inline-"))
            .ok_or("plugin hook missing")?;
        assert_eq!(command.directory, fixture.workspace);
        assert!(command
            .command
            .last()
            .is_some_and(|text| text.ends_with("echo plugin")));
    }
    let start = settings.commands(Event::SessionStart);
    assert_eq!(
        start.first().map(|command| command.name.as_str()),
        Some("user-first")
    );
    assert_eq!(
        start.last().map(|command| command.name.as_str()),
        Some("project-last")
    );
    assert_eq!(HookSettings::from_catalog(&sources)?, settings);
    for hooks in [
        json!({"SessionStart":[{"hooks":[{"type":"prompt","prompt":"Judge"}]}]}),
        json!({"SessionStart":[{"matcher":"resume","hooks":[{"type":"command","command":"echo skipped"}]}]}),
        json!({"SessionStart":[{"hooks":[{"type":"command","command":"echo bad","async":true}]}]}),
        json!({"SessionStart":[{"hooks":[{"type":"command","command":"echo ${CLAUDE_ENV_FILE}"}]}]}),
    ] {
        write(
            &fixture.market.join("two/hooks/hooks.json"),
            &json!({"hooks":hooks}).to_string(),
        )?;
        assert!(fixture
            .operation(json!({"action":"install","plugin":"two@local"}))
            .is_err());
        assert_eq!(loaded(&fixture.sources()?)?.len(), 1);
    }
    Ok(())
}

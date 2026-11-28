use super::*;
use crate::dogfood::{production::tests::projection_target, ServerApplication};
use grapt_types::{ClockAuthority, DeterministicAuthorities, InstructionKey, ProjectId};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
#[path = "ingress.rs"]
mod ingress;
#[path = "../sandbox_worktree_api_tests.rs"]
mod sandbox_worktree_api_tests;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Root(PathBuf);
impl Root {
    fn new() -> TestResult<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = Self(std::env::temp_dir().join(format!(
            "grapt-session-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        for name in ["service", "project", "private"] {
            std::fs::create_dir_all(root.0.join(name))?;
        }
        Ok(root)
    }
    fn config(&self, file: &str, session: &SessionId, agent: &str) -> TestResult<PathBuf> {
        let path = self.0.join(file);
        std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
        grapt_login::profile_security::protect_directory(path.parent().ok_or("parent")?)?;
        let temporary = path.with_extension("tmp");
        std::fs::write(
            &temporary,
            serde_json::to_vec(&json!({"mcp":{"stateengine":{
            "type":"local", "enabled":false, "command":["node","server.mjs"], "environment":{
                "STATEENGINE_PROJECT_ID":"fixture", "STATEENGINE_AGENT_ID":agent,
                "SECODE_SESSION_ID":session.as_str()}}}}))?,
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
        }
        let opened = std::fs::File::open(&temporary)?;
        grapt_login::profile_security::protect_new_private_file(&temporary, &opened)?;
        drop(opened);
        std::fs::rename(temporary, &path)?;
        Ok(path)
    }
    fn open(
        &self,
    ) -> TestResult<(
        Arc<ServerApplication>,
        crate::dogfood::prompt_driver::PromptDriver,
    )> {
        Ok(crate::dogfood::test_provider::open(
            &self.0.join("service"),
            grapt_store::EventStore::open(&self.0.join("service/events.db"), "session-config")?,
            &projection_target()?,
            crate::dogfood::event_hub()?,
            crate::dogfood::plugins::PluginRuntime::empty(),
        )?)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

fn session(root: &Root, app: &Arc<ServerApplication>, agent: &str) -> TestResult<SessionId> {
    let authorities = DeterministicAuthorities::new(&*app.clock, &*app.ids);
    let seed = grapt_session::SessionSeed::new(
        ProjectId::new("prj_config").ok_or("project")?,
        root.0.join("project").to_str().ok_or("path")?,
        "config witness",
    )?;
    let session = grapt_runtime::SessionPorts::new(app.inbox.clone(), &authorities)
        .create(&seed)?
        .session;
    InstructionService::put_entry(
        app.lock_inbox().store_mut(),
        &session,
        &InstructionKey::new("stateengine-identity").ok_or("key")?,
        &json!({"scope":"project","project_id":"fixture","agent_id":agent}).to_string(),
        app.clock.now().as_millis(),
    )?;
    Ok(session)
}

async fn bind(
    app: &Arc<ServerApplication>,
    session: &SessionId,
    path: &Path,
    success: bool,
) -> TestResult {
    let body = json!({"capability":"fixture-private", "configPath":path.to_str().ok_or("path")?})
        .to_string();
    let response = ingress::call(
        app,
        "PUT",
        &format!("/api/session/{session}/stateengine/capability"),
        &body,
    )
    .await?;
    assert!(
        response.starts_with(if success {
            b"HTTP/1.1 204"
        } else {
            b"HTTP/1.1 400"
        }),
        "wrong authenticated bind status: {}",
        String::from_utf8_lossy(&response)
    );
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn authenticated_config_binding_survives_restart_without_cross_seat_inheritance() -> TestResult
{
    let root = Root::new()?;
    let (app, driver) = root.open()?;
    let alice = session(&root, &app, "alice")?;
    let bob = session(&root, &app, "bob")?;
    let path = root.config("private/alice.json", &alice, "alice")?;
    bind(&app, &alice, &path, true).await?;
    let prior =
        config_binding::read(app.lock_inbox().store().connection(), &alice)?.ok_or("binding")?;
    let published = sources(
        &app.inbox,
        &alice,
        &root.0.join("service"),
        &root.0.join("project"),
    )?;
    assert_eq!(
        published.documents.last().ok_or("document")?.source,
        path.to_string_lossy()
    );
    let bob_sources = sources(
        &app.inbox,
        &bob,
        &root.0.join("service"),
        &root.0.join("project"),
    )?;
    assert!(!bob_sources.document_paths.contains(&path));
    assert_ne!(
        registry_key(&app.inbox, &alice, "same-directory")?,
        registry_key(&app.inbox, &bob, "same-directory")?
    );
    // A mismatched selected file cannot revoke or replace the old binding.
    let wrong = root.config("private/wrong.json", &bob, "alice")?;
    let prior_capability = app
        .session_capabilities
        .lock()
        .map_err(|_| "custody")?
        .values
        .get(&alice)
        .cloned()
        .ok_or("prior capability")?;
    bind(&app, &alice, &wrong, false).await?;
    let wrong_agent = root.config("private/wrong-agent.json", &alice, "bob")?;
    bind(&app, &alice, &wrong_agent, false).await?;
    let wrong_project = root.config("private/wrong-project.json", &alice, "alice")?;
    let mut wrong_project_json: Value = serde_json::from_slice(&std::fs::read(&wrong_project)?)?;
    wrong_project_json["mcp"]["stateengine"]["environment"]["STATEENGINE_PROJECT_ID"] =
        json!("foreign");
    std::fs::write(&wrong_project, serde_json::to_vec(&wrong_project_json)?)?;
    bind(&app, &alice, &wrong_project, false).await?;
    assert!(
        Arc::ptr_eq(
            &prior_capability,
            app.session_capabilities
                .lock()
                .map_err(|_| "custody")?
                .values
                .get(&alice)
                .ok_or("retained capability")?
        ),
        "failed config bind revoked private custody"
    );
    assert_eq!(
        config_binding::read(app.lock_inbox().store().connection(), &alice)?,
        Some(prior.clone())
    );
    let prior_registry_key = registry_key(
        &app.inbox,
        &alice,
        root.0.join("project").to_str().ok_or("path")?,
    )?;
    let replacement = root.config("private/replacement.json", &alice, "alice")?;
    bind(&app, &alice, &replacement, true).await?;
    let replaced = config_binding::read(app.lock_inbox().store().connection(), &alice)?
        .ok_or("replacement")?;
    assert!(replaced.sequence > prior.sequence, "rebind was not audited");
    assert_ne!(
        prior_registry_key,
        registry_key(
            &app.inbox,
            &alice,
            root.0.join("project").to_str().ok_or("path")?
        )?,
        "rebound transport shares an old declaration namespace"
    );
    assert!(
        validate_registry_key(&app.lock_inbox(), &alice, &prior_registry_key).is_err(),
        "old route received replacement capability custody"
    );
    let current_key = registry_key(
        &app.inbox,
        &alice,
        root.0.join("project").to_str().ok_or("path")?,
    )?;
    validate_registry_key(&app.lock_inbox(), &alice, &current_key)?;
    let mut stale_published = false;
    assert!(
        publish_current(&app.inbox, &alice, Some(prior.sequence), || {
            stale_published = true
        })
        .is_err()
    );
    assert!(
        !stale_published,
        "stale asynchronous work published the old config"
    );
    let mut current_published = false;
    publish_current(&app.inbox, &alice, Some(replaced.sequence), || {
        current_published = true
    })?;
    assert!(current_published);
    drop(driver);
    drop(app);
    let (app, _driver) = root.open()?;
    assert_eq!(
        config_binding::read(app.lock_inbox().store().connection(), &alice)?,
        Some(replaced)
    );
    assert!(config_binding::read(app.lock_inbox().store().connection(), &bob)?.is_none());
    assert_eq!(
        sources(
            &app.inbox,
            &alice,
            &root.0.join("service"),
            &root.0.join("project")
        )?
        .documents
        .last()
        .ok_or("reopened document")?
        .source,
        replacement.to_string_lossy()
    );
    // An atomic replacement is reread, and invalid content never falls back.
    root.config("private/replacement.json", &bob, "bob")?;
    assert!(
        sources(
            &app.inbox,
            &alice,
            &root.0.join("service"),
            &root.0.join("project")
        )
        .is_err(),
        "mismatched atomically replaced configuration fell back"
    );
    Ok(())
}

#[tokio::test]
async fn configured_bad_paths_and_file_protection_refuse_before_binding() -> TestResult {
    let root = Root::new()?;
    let (app, _driver) = root.open()?;
    let alice = session(&root, &app, "alice")?;
    for name in ["project/repository.json", "private/.grapt/config.json"] {
        let path = root.config(name, &alice, "alice")?;
        bind(&app, &alice, &path, false).await?;
    }
    let other_repository = root.config("private/other-repository/config.json", &alice, "alice")?;
    std::fs::write(
        root.0.join("private/other-repository/.git"),
        b"gitdir: /unused",
    )?;
    bind(&app, &alice, &other_repository, false).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = root.config("private/loose.json", &alice, "alice")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        bind(&app, &alice, &path, false).await?;
    }
    #[cfg(unix)]
    {
        let name = format!(
            "private/{}config.json",
            "x/".repeat(catalog_config::MAX_ANCESTORS)
        );
        let deep = root.config(&name, &alice, "alice")?;
        bind(&app, &alice, &deep, false).await?;
    }
    bind(&app, &alice, Path::new("relative.json"), false).await?;
    bind(&app, &alice, &root.0.join("private/absent.json"), false).await?;
    assert!(config_binding::read(app.lock_inbox().store().connection(), &alice)?.is_none());
    Ok(())
}

#[tokio::test]
async fn mcp_inspection_uses_the_selected_session_and_refuses_stale_or_mismatched_configuration(
) -> TestResult {
    let root = Root::new()?;
    let (app, _driver) = root.open()?;
    let alice = session(&root, &app, "alice")?;
    let bob = session(&root, &app, "bob")?;
    let path = root.config("private/alice.json", &alice, "alice")?;
    bind(&app, &alice, &path, true).await?;
    for (session, visible) in [(&alice, true), (&bob, false)] {
        for route in ["/api/mcp", "/api/mcp/inspect"] {
            let response =
                ingress::call(&app, "GET", &format!("{route}?session={session}"), "").await?;
            let response = String::from_utf8(response)?;
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert_eq!(
                response.contains("stateengine"),
                visible,
                "cross-Session catalog: {response}"
            );
        }
    }
    for query in [
        format!("session={alice}&location[directory]=/different"),
        "session=ses_missing".to_owned(),
        "session=not-a-session".to_owned(),
        format!("session={alice}&session={bob}"),
    ] {
        let response = ingress::call(&app, "GET", &format!("/api/mcp/inspect?{query}"), "").await?;
        assert!(
            response.starts_with(b"HTTP/1.1 400"),
            "invalid selector was accepted"
        );
    }
    let query = serde_json::from_value(json!({"session":alice.as_str()}))?;
    let stale = catalog::select(&app, &query).await?;
    let replacement = root.config("private/replacement.json", &alice, "alice")?;
    bind(&app, &alice, &replacement, true).await?;
    assert!(
        stale.validate_current(&app).is_err(),
        "old MCP inspection survived rebind"
    );
    root.config("private/replacement.json", &bob, "bob")?;
    let response = ingress::call(
        &app,
        "GET",
        &format!("/api/mcp/inspect?session={alice}"),
        "",
    )
    .await?;
    assert!(
        response.starts_with(b"HTTP/1.1 400"),
        "invalid bound file fell back to service config"
    );
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn scoped_content_keeps_startup_directory_and_refresh_namespace_distinct() -> TestResult {
    use grapt_api::u3a::{
        CommandInspectionResponseDto, FormListResponseDto, FormReplyRequestDto,
        McpContentRequestDto, McpContentResponseDto, McpResourcesResponseDto,
    };
    let root = Root::new()?;
    let (app, _driver) = root.open()?;
    let alice = session(&root, &app, "alice")?;
    let bob = session(&root, &app, "bob")?;
    let mut alice_peer = ingress::content_peer("alice", true).await?;
    let bob_peer = ingress::content_peer("bob", false).await?;
    for (owner, label, peer) in [(&alice, "alice", &alice_peer), (&bob, "bob", &bob_peer)] {
        let path = root.config(&format!("private/{label}.json"), owner, label)?;
        let mut config: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        config["mcp"]["content-server"] = json!({"type":"remote","url":peer.url,"enabled":true});
        std::fs::write(&path, serde_json::to_vec(&config)?)?;
        bind(&app, owner, &path, true).await?;
    }
    let directory = root.0.join("project");
    let location = grapt_types::LocationRef::new(directory.to_str().ok_or("cwd")?, None)?;
    let cwd = location.directory();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("location[directory]", cwd)
        .finish();
    let (host, recipient) = ingress::approver(&app, &query).await?;
    let discover_path = format!("/api/command/inspect?session={alice}");
    let answer = async {
        for _ in 0..100 {
            let forms: FormListResponseDto = ingress::page(
                &ingress::call(&app, "GET", &format!("/api/form/request?{query}"), "").await?,
            )?;
            assert_eq!(forms.location().get().directory().get().as_str(), cwd);
            if let Some(form) = forms
                .data()
                .get()
                .as_slice()
                .iter()
                .find(|form| form.session_id().get().as_str() == "global")
            {
                let info = serde_json::to_value(form)?;
                assert_eq!(info["metadata"]["message"], "Alice startup");
                assert_eq!(info["metadata"]["startup"], true);
                let id = form.id().get().as_inner();
                let request: FormReplyRequestDto =
                    serde_json::from_value(json!({"answer":{"name":"Alice"}}))?;
                let reply = ingress::call(
                    &app,
                    "POST",
                    &format!("/api/session/global/form/{id}/reply?{query}"),
                    &serde_json::to_string(&request)?,
                )
                .await?;
                assert!(
                    reply.starts_with(b"HTTP/1.1 204"),
                    "{}",
                    String::from_utf8_lossy(&reply)
                );
                return Ok::<_, Box<dyn std::error::Error>>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        Err("startup form was not available through the cwd API".into())
    };
    let (discovered, answered) =
        tokio::join!(ingress::call(&app, "GET", &discover_path, ""), answer);
    answered?;
    let alice_commands: CommandInspectionResponseDto = ingress::page(&discovered?)?;
    let bob_commands: CommandInspectionResponseDto = ingress::page(
        &ingress::call(
            &app,
            "GET",
            &format!("/api/command/inspect?session={bob}"),
            "",
        )
        .await?,
    )?;
    for (owner, commands, label) in [
        (&alice, &alice_commands, "alice"),
        (&bob, &bob_commands, "bob"),
    ] {
        let prompts: Vec<_> = commands
            .data()
            .get()
            .as_slice()
            .iter()
            .filter_map(|row| row.mcp().as_present())
            .map(|binding| binding.name().get().as_str())
            .collect();
        assert_eq!(prompts, [format!("{label}_prompt")]);
        assert_eq!(commands.location().get().directory().get().as_str(), cwd);
        let resources: McpResourcesResponseDto = ingress::page(
            &ingress::call(
                &app,
                "GET",
                &format!("/api/mcp/resource?session={owner}"),
                "",
            )
            .await?,
        )?;
        let names: Vec<_> = resources
            .data()
            .get()
            .resources()
            .get()
            .as_slice()
            .iter()
            .map(|row| row.name().get().as_str())
            .collect();
        assert_eq!(names, [format!("{label}_resource")]);
        assert_eq!(resources.location().get().directory().get().as_str(), cwd);
    }
    let cwd_commands: CommandInspectionResponseDto = ingress::page(
        &ingress::call(&app, "GET", &format!("/api/command/inspect?{query}"), "").await?,
    )?;
    let scope = |commands: &CommandInspectionResponseDto| {
        commands
            .mcp_scope()
            .as_present()
            .map(|value| value.as_str().to_owned())
            .ok_or("scope")
    };
    let alice_scope = scope(&alice_commands)?;
    assert_ne!(alice_scope, scope(&bob_commands)?);
    assert_ne!(alice_scope, scope(&cwd_commands)?);
    // Initial connection hints cannot satisfy the notification assertion.
    let refresh = app.hub.subscribe_host()?;
    let request: McpContentRequestDto =
        serde_json::from_value(json!({"action":"read_resource","uri":"fixture://content"}))?;
    let content: McpContentResponseDto = ingress::page(
        &ingress::call(
            &app,
            "POST",
            &format!("/api/mcp/content-server/content?session={alice}"),
            &serde_json::to_string(&request)?,
        )
        .await?,
    )?;
    let McpContentResponseDto::UserContent { location, content } = content else {
        return Err("resource read did not return user content".into());
    };
    assert_eq!(location.directory().get().as_str(), cwd);
    assert!(content.text.as_str().contains("alice private content"));
    assert!(!content.text.as_str().contains("bob private content"));
    refresh.cancel();
    let mut scopes = Vec::new();
    for _ in 0..16 {
        let Some(frame) = refresh.next_frame()? else {
            break;
        };
        let text = std::str::from_utf8(&frame)?;
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data:"))
            .ok_or("SSE data")?;
        let event: Value = serde_json::from_str(data.trim())?;
        if event["type"] == "mcp.catalog.changed" {
            scopes.push(
                event["data"]["scope"]
                    .as_str()
                    .ok_or("refresh scope")?
                    .to_owned(),
            );
        }
    }
    assert_eq!(scopes, [alice_scope]);
    alice_peer.finish().await?;
    drop(recipient);
    assert!(matches!(
        host.stop().await,
        grapt_api::ApiHostExit::StoppedGracefully { .. }
    ));
    Ok(())
}

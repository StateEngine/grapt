//! Pinned execution: only admitted module bytes, runtime bytes, and built-ins
//! may execute, and guest output cannot corrupt the protocol.

use super::*;

fn tool_error(
    supervisor: &PluginSupervisor,
    generation: &grapt_plugin::PluginGeneration,
    name: &str,
) -> Result<String, Box<dyn Error>> {
    let registration = generation.registrations().first().ok_or("registration")?;
    let tool = registration
        .tools
        .iter()
        .find(|tool| tool.name == name)
        .ok_or("tool")?;
    match supervisor.invoke(
        generation.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    ) {
        Ok(value) => Err(format!("{name} unexpectedly succeeded: {value}").into()),
        Err(error) => Ok(error.to_string()),
    }
}

#[test]
fn pinned_loader_refuses_replaced_unpinned_outside_and_denied_modules() -> Result<(), Box<dyn Error>>
{
    let (root, plugin) = fixture(
        "pinned-loader",
        r#"
const probe = (name, load) => ({ name, args: {type:"object"}, async execute() { return { value: await load() } } })
export default {
  id: "fixture.pinned",
  async setup(ctx) {
    await ctx.tool.transform((draft) => {
      draft.add(probe("replaced", async () => (await import("./dep.mjs")).value))
      draft.add(probe("unpinned", async () => (await import("./later.mjs")).value))
      draft.add(probe("outside", async () => (await import("../outside-module.mjs")).value))
      draft.add(probe("child_process", async () => typeof (await import("node:child_process")).spawn))
      draft.add(probe("module_escape", async () => typeof process.getBuiltinModule("node:module").registerHooks))
      draft.add(probe("data_url", async () => (await import("data:text/javascript,export default 1")).default))
      draft.add(probe("cjs_replaced", async () => (await import("./cjs-loader.cjs")).default.dep()))
      draft.add(probe("cjs_child_process", async () => (await import("./cjs-loader.cjs")).default.child()))
    })
  }
}
"#,
    )?;
    std::fs::write(root.join("dep.mjs"), "export const value = 'admitted'\n")?;
    std::fs::write(
        root.join("dep.cjs"),
        "module.exports = { value: 'admitted' }\n",
    )?;
    std::fs::write(
        root.join("cjs-loader.cjs"),
        "module.exports = { dep: () => require('./dep.cjs').value, child: () => typeof require('child_process').spawn }\n",
    )?;
    let outside = root
        .parent()
        .ok_or("fixture root has no parent")?
        .join("outside-module.mjs");
    std::fs::write(&outside, "export const value = 'outside'\n")?;
    let declarations = declarations(&plugin)?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let generation = gated_activate(&supervisor, declarations, serde_json::json!({}))?;

    std::fs::write(root.join("dep.mjs"), "export const value = 'substituted'\n")?;
    std::fs::write(
        root.join("dep.cjs"),
        "module.exports = { value: 'substituted' }\n",
    )?;
    std::fs::write(root.join("later.mjs"), "export const value = 'unpinned'\n")?;
    for (name, expected) in [
        ("replaced", "changed after trust validation"),
        ("unpinned", "does not resolve inside its admitted snapshot"),
        ("outside", "does not resolve inside its admitted snapshot"),
        ("child_process", "denied built-in node:child_process"),
        ("module_escape", "denied built-in node:module"),
        ("data_url", "import scheme is not admitted"),
        ("cjs_replaced", "changed after trust validation"),
        ("cjs_child_process", "denied built-in"),
    ] {
        let error = tool_error(&supervisor, &generation, name)?;
        assert!(
            error.contains(expected),
            "{name} was not refused for the right reason: {error}"
        );
    }
    supervisor.shutdown()?;
    std::fs::remove_file(outside)?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn runtime_and_bridge_bytes_are_verified_before_any_execution() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "runtime-pins",
        "export default {id:'fixture.runtime',setup(){}}\n",
    )?;
    let (mut wrong, _wrong_root_cleanup) = runtime()?;
    wrong.expected.bridge_sha256 = "0".repeat(64);
    let refused = PluginSupervisor::new(wrong, DEFAULT_BRIDGE_LIMITS);
    assert!(
        matches!(&refused, Err(PluginError::Runtime(message)) if message.contains("bridge asset digest does not match")),
        "a mismatched bridge pin was not refused before execution: {refused:?}"
    );

    let (admitted, _admitted_root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(admitted.clone(), DEFAULT_BRIDGE_LIMITS)?;
    let copy = admitted
        .private_root
        .join(&admitted.expected.executable_sha256[..16])
        .join(&admitted.expected.bridge_sha256[..16])
        .join("bridge.mjs");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600))?;
        std::fs::write(&copy, "process.exit(0)\n")?;
        let tampered = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}));
        assert!(
            matches!(&tampered, Err(PluginError::Runtime(message)) if message.contains("bridge asset digest does not match")),
            "a replaced private bridge copy was executed: {tampered:?}"
        );
    }
    #[cfg(windows)]
    {
        let replaced = std::fs::write(&copy, "process.exit(0)\n");
        assert!(
            replaced.is_err(),
            "the held private bridge copy accepted a write while its supervisor was live"
        );
        let generation =
            gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}))?;
        assert!(supervisor.current_is_running(generation.id())?);
    }
    supervisor.shutdown()?;
    drop(supervisor);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn guest_console_output_cannot_corrupt_the_protocol() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "console",
        r#"
export default {
  id: "fixture.console",
  async setup(ctx) {
    console.log("guest console noise")
    console.error({ noisy: true })
    process.stdout.write("{\"version\":2,\"generation\":1,\"correlation\":999,\"outcome\":\"ok\"}\n")
    await ctx.tool.transform((draft) => draft.add({
      name: "noisy",
      args: {type:"object"},
      async execute() { console.log("more noise"); return {output:"clean"} }
    }))
  }
}
"#,
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let generation = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}))?;
    let registration = generation.registrations().first().ok_or("registration")?;
    let tool = registration.tools.first().ok_or("tool")?;
    let output = supervisor.invoke(
        generation.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(output["output"], "clean");
    assert!(supervisor.current_is_running(generation.id())?);
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn the_bridge_enforces_host_sent_registration_limits() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "registration-limit",
        r#"
export default {
  id: "fixture.limited",
  async setup(ctx) {
    await ctx.tool.hook("execute.before", () => {})
    await ctx.tool.hook("execute.after", () => {})
    await ctx.session.hook("context", () => {})
  }
}
"#,
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(
        runtime,
        BridgeLimits {
            registrations: 2,
            ..DEFAULT_BRIDGE_LIMITS
        },
    )?;
    let refused = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}));
    assert!(
        matches!(&refused, Err(PluginError::Invocation(message)) if message.contains("callback registrations exceed the bound")),
        "the bridge ignored the host-sent registration limit: {refused:?}"
    );
    assert_eq!(supervisor.live_processes(), 0);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[derive(Debug, Default)]
struct OwnerHost {
    seen: Mutex<Vec<(String, String)>>,
}

impl PluginHost for OwnerHost {
    fn call(
        &self,
        context: &PluginHostContext,
        _operation: HostOperation,
        _payload: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.seen
            .lock()
            .map_err(|_| "owner host poisoned".to_owned())?
            .push((context.plugin().to_owned(), context.owner().to_owned()));
        Ok(serde_json::Value::Null)
    }
}

impl OwnerHost {
    fn last_owner(&self) -> Result<String, Box<dyn Error>> {
        let seen = self.seen.lock().map_err(|_| "owner host poisoned")?;
        let (plugin, owner) = seen.last().ok_or("the plugin made no host call")?;
        assert_eq!(plugin, "fixture.shared-id");
        Ok(owner.clone())
    }
}

#[test]
fn durable_owner_identity_is_the_admitted_source_not_the_guest_id() -> Result<(), Box<dyn Error>> {
    let (root, _) = fixture(
        "owner-identity",
        "export default {id:'unused',setup(){}}
",
    )?;
    let package = |name: &str, body: &str| -> Result<PathBuf, Box<dyn Error>> {
        let path = root.join(name).join("plugin.mjs");
        std::fs::create_dir_all(path.parent().ok_or("plugin parent")?)?;
        std::fs::write(
            &path,
            format!("export default {{id:'fixture.shared-id',async setup(ctx){{ {body}; await ctx.session.get({{}}).catch(()=>{{}}) }}}}
"),
        )?;
        Ok(path)
    };
    let host = Arc::new(OwnerHost::default());
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::with_host(
        runtime,
        DEFAULT_BRIDGE_LIMITS,
        Arc::clone(&host) as Arc<dyn PluginHost>,
    )?;
    let first = package("first", "const version = 1")?;
    gated_activate(&supervisor, declarations(&first)?, serde_json::json!({}))?;
    let first_owner = host.last_owner()?;

    let impostor = package("impostor", "const version = 1")?;
    gated_activate(&supervisor, declarations(&impostor)?, serde_json::json!({}))?;
    let impostor_owner = host.last_owner()?;
    assert_ne!(
        impostor_owner, first_owner,
        "a different package claiming the same plugin id inherited the first package's durable owner"
    );

    package("first", "const version = 2")?;
    gated_activate(&supervisor, declarations(&first)?, serde_json::json!({}))?;
    assert_eq!(
        host.last_owner()?,
        first_owner,
        "a reloaded package lost its durable owner across a successor generation"
    );
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

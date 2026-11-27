use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(target_os = "linux")]
use grapt_plugin::MEMORY_MAX_HEADROOM_BYTES;
use grapt_plugin::{
    resolve_declarations, BridgeLimits, BridgeRuntime, CallAuthority, ConfigDocument,
    HostOperation, PluginError, PluginGenerationId, PluginHost, PluginHostContext, PluginOrigin,
    PluginSupervisor, RuntimePin, DEFAULT_BRIDGE_LIMITS,
};
use sha2::{Digest, Sha256};

fn node() -> Result<PathBuf, Box<dyn Error>> {
    let candidate = if let Some(path) = std::env::var_os("NODE") {
        PathBuf::from(path)
    } else {
        let (program, argument) = if cfg!(windows) {
            ("where.exe", "node.exe")
        } else {
            ("which", "node")
        };
        let output = Command::new(program).arg(argument).output()?;
        if !output.status.success() {
            return Err("Node is required for the plugin bridge witness".into());
        }
        let text = String::from_utf8(output.stdout)?;
        let first = text
            .lines()
            .next()
            .ok_or("Node lookup returned no executable")?;
        PathBuf::from(first)
    };
    // The PATH entry may be a launcher chain that execs the real runtime (a
    // package-manager shim, a version manager, a symlink farm). The
    // supervisor pins the executable's bytes and proves the owned unit runs
    // that file, so the witness must resolve the launcher to
    // the runtime it actually execs rather than pin the launcher itself.
    let output = Command::new(&candidate)
        .arg("-e")
        .arg(
            r#"if (process.versions.node.split('.')[0] !== '24') {
                throw new Error(`plugin tests require Node 24, found ${process.version}; install Node 24 and set NODE to its executable`)
            }
            process.stdout.write(process.execPath)"#,
        )
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "node candidate {candidate:?} did not run: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let resolved = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    if !resolved.is_absolute() || !resolved.is_file() {
        return Err(format!(
            "node candidate {candidate:?} resolved to {resolved:?}, which is not an absolute file"
        )
        .into());
    }
    Ok(resolved)
}

fn bridge() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/plugin-bridge/index.mjs")
}

// Each fixture root is the test's own obligation: this guard removes it
// when the test scope ends, so a failed or early-returned witness can
// never leave a private runtime directory on the host. It is independent
// of the product's admission cleanup, which the dropped-supervisor
// witnesses below.
use crate::support;

struct RootCleanup(PathBuf);

impl Drop for RootCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(support::recovery_root(&self.0));
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "plugin fixture could not clean up its private root {}: {error}",
                    self.0.display()
                );
            }
        }
    }
}

fn runtime() -> Result<(BridgeRuntime, RootCleanup), Box<dyn Error>> {
    runtime_for(&bridge())
}

fn runtime_for(bridge: &Path) -> Result<(BridgeRuntime, RootCleanup), Box<dyn Error>> {
    // A tamper witness must never replace another supervisor's runtime bytes.
    static NEXT_RUNTIME: AtomicUsize = AtomicUsize::new(0);
    let runtime_id = NEXT_RUNTIME
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            (next < 4096).then_some(next + 1)
        })
        .map_err(|_| "plugin fixture runtime limit exhausted")?;
    let executable = node()?;
    let version = String::from_utf8(Command::new(&executable).arg("--version").output()?.stdout)?
        .trim()
        .to_owned();
    let private_root = std::env::temp_dir().join(format!(
        "secode-plugin-runtime-{}-{runtime_id}",
        std::process::id()
    ));
    Ok((
        BridgeRuntime {
            expected: RuntimePin {
                version,
                executable_sha256: sha256(&executable)?,
                bridge_sha256: sha256(bridge)?,
            },
            executable,
            bridge: bridge.to_path_buf(),
            recovery: support::recovery(&private_root),
            private_root: private_root.clone(),
        },
        RootCleanup(private_root),
    ))
}

#[test]
fn fixture_runtimes_have_distinct_private_roots() -> Result<(), Box<dyn Error>> {
    let (first, _first_cleanup) = runtime()?;
    let (second, _second_cleanup) = runtime()?;
    assert_ne!(first.private_root, second.private_root);
    Ok(())
}

/// The supervisor removes the private runtime copies it admitted when the
/// last supervisor claiming the directory drops; the host root is removed
/// with the empty levels the admission created. Witness for the runtime-dir
/// leak: without the fix every dropped supervisor left its admit directory
/// (about 121 MB per admitted runtime) on the host forever.
#[test]
fn a_dropped_supervisor_removes_its_private_runtime_directory() -> Result<(), Box<dyn Error>> {
    let (runtime, _root_cleanup) = runtime()?;
    let root = runtime.private_root.clone();
    let leaf = root
        .join(&runtime.expected.executable_sha256[..16])
        .join(&runtime.expected.bridge_sha256[..16]);
    assert!(
        !root.exists(),
        "fixture runtime root must start absent: {root:?}"
    );
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    assert!(
        leaf.exists(),
        "the admitted runtime copies are not materialized while the supervisor is alive: {leaf:?}"
    );
    drop(supervisor);
    assert!(
        !root.exists(),
        "the dropped supervisor left its private runtime directory behind: {root:?}"
    );
    Ok(())
}

/// A private runtime directory is shared by every supervisor that admits the
/// same pin into the same root, so the copies must outlive every supervisor
/// but the last one dropped.
#[test]
fn a_shared_runtime_directory_survives_until_the_last_supervisor_drops(
) -> Result<(), Box<dyn Error>> {
    let (first, _first_root_cleanup) = runtime()?;
    let root = first.private_root.clone();
    let leaf = root
        .join(&first.expected.executable_sha256[..16])
        .join(&first.expected.bridge_sha256[..16]);
    let shared = BridgeRuntime {
        expected: first.expected.clone(),
        executable: first.executable.clone(),
        bridge: first.bridge.clone(),
        private_root: root.clone(),
        recovery: first.recovery.clone(),
    };
    let first_supervisor = PluginSupervisor::new(first, DEFAULT_BRIDGE_LIMITS)?;
    let second_supervisor = PluginSupervisor::new(shared, DEFAULT_BRIDGE_LIMITS)?;
    drop(first_supervisor);
    assert!(
        leaf.exists(),
        "the shared runtime copies vanished while a supervisor still uses them: {leaf:?}"
    );
    drop(second_supervisor);
    assert!(
        !root.exists(),
        "the last supervisor's drop left the shared runtime directory behind: {root:?}"
    );
    Ok(())
}

fn custom_bridge(root: &Path, label: &str, source: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = root.join(format!("bridge-{label}.mjs"));
    std::fs::write(&path, source)?;
    Ok(path)
}

fn sha256(path: &Path) -> Result<String, Box<dyn Error>> {
    let digest = Sha256::digest(std::fs::read(path)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn oracle_effect_package() -> Result<PathBuf, Box<dyn Error>> {
    let oracle = std::env::var_os("GRAPT_ORACLE_ROOT")
        .map(PathBuf::from)
        .ok_or("set GRAPT_ORACLE_ROOT to the pinned oracle checkout")?;
    let output = Command::new(node()?)
        .args(["-e", "console.log(require.resolve('effect'))"])
        .current_dir(oracle.join("packages/core"))
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "pinned oracle Effect package is unavailable: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let resolved = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    resolved
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .ok_or_else(|| "resolved Effect package path has no package root".into())
}

fn fixture(label: &str, source: &str) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("secode-plugin-{}-{label}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    let plugin = root.join("fixture.mjs");
    std::fs::write(&plugin, source)?;
    Ok((root, plugin))
}

fn declarations(path: &Path) -> Result<Vec<grapt_plugin::PluginDeclaration>, Box<dyn Error>> {
    let parent = path.parent().ok_or("fixture has no parent")?;
    let file = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("name")?;
    let resolved = resolve_declarations(&[ConfigDocument {
        directory: parent.to_path_buf(),
        source: "explicit-fixture".to_owned(),
        origin: PluginOrigin::LaunchExplicit,
        value: serde_json::json!({"plugins":[{"package":format!("./{file}"),"options":{"prefix":"ok"}}]}),
    }])?;
    if !resolved.blocked.is_empty() {
        return Err(format!("fixture declaration was blocked: {:?}", resolved.blocked).into());
    }
    Ok(resolved.active)
}

/// The bounded fixture-activation gate (PLR-R10): the source-pinning
/// failures measured on this carrier (P4 class) were a function of how
/// many fixture activations ran concurrently inside one test binary -
/// 10 concurrent activations measured green (pin-probe-wave10: 10/10
/// ok, max 1.73 s) and 30 measured red (26/30 pinning timeouts) - not
/// of the product. The gate bounds the unrelated fixture pressure to
/// the measured-green width and leaves the failure that the product
/// owns to the product's own per-exchange bound (10 s
/// startup_millis). It is test-only: no product bound changed, the
/// wait is bounded (300 s cap, RAII release on every exit path, panic
/// names the stall instead of hanging), and the product-envelope
/// witness runs in its own binary (tests/envelope.rs) where it never
/// takes this permit, so it proves the real envelope under zero
/// fixture pressure. The permit is per activate() call - one per
/// generation admission, whatever the exchange declares - not per unit
/// and not per plugin: the measured width counts concurrent
/// activations, and the product's own per-exchange bound applies
/// inside each held slot.
const MAX_CONCURRENT_FIXTURE_ACTIVATIONS: usize = 10;
const FIXTURE_ACTIVATION_WAIT_TICKS: u32 = 3_000; // 300 s cap at 100 ms ticks

static FIXTURE_ACTIVATION_SLOTS: AtomicUsize = AtomicUsize::new(0);

struct FixtureActivationPermit;

impl FixtureActivationPermit {
    fn acquire() -> Self {
        for _ in 0..FIXTURE_ACTIVATION_WAIT_TICKS {
            if FIXTURE_ACTIVATION_SLOTS
                .fetch_update(Ordering::Acquire, Ordering::Relaxed, |held| {
                    (held < MAX_CONCURRENT_FIXTURE_ACTIVATIONS).then_some(held + 1)
                })
                .is_ok()
            {
                return FixtureActivationPermit;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("the bounded fixture-activation wait outlived its 300 s cap: every activation slot stalled");
    }
}

impl Drop for FixtureActivationPermit {
    fn drop(&mut self) {
        FIXTURE_ACTIVATION_SLOTS.fetch_sub(1, Ordering::Release);
    }
}

fn gated_activate(
    supervisor: &PluginSupervisor,
    declarations: Vec<grapt_plugin::PluginDeclaration>,
    app: serde_json::Value,
) -> Result<std::sync::Arc<grapt_plugin::PluginGeneration>, PluginError> {
    let _permit = FixtureActivationPermit::acquire();
    supervisor.activate(declarations, app)
}

#[test]
fn external_tool_uses_one_generation_bound_callback() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "tool",
        r#"
export default {
  id: "fixture.external",
  async setup(ctx) {
    await ctx.tool.transform((draft) => draft.add({
      name: "fixture_echo",
      description: "bounded external echo",
      args: {type:"object",additionalProperties:false,properties:{text:{type:"string"}},required:["text"]},
      async execute(input, context) {
        return { title: ctx.options.prefix, output: `${input.text}:${context.sessionID}`, metadata: {} }
      }
    }))
  }
}
"#,
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let generation = gated_activate(
        &supervisor,
        declarations(&plugin)?,
        serde_json::json!({"name":"Grapt","version":"fixture"}),
    )?;
    let registration = generation
        .registrations()
        .first()
        .ok_or("registration is absent")?;
    let tool = registration.tools.first().ok_or("tool is absent")?;
    let result = supervisor.invoke(
        generation.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({"text":"hello"}),
        serde_json::json!({"sessionID":"ses_fixture"}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(result["output"], "hello:ses_fixture");
    let stale = PluginGenerationId::new(generation.id().value() + 1).ok_or("generation")?;
    assert!(
        matches!(
            supervisor.invoke(
                stale,
                &registration.plugin_id,
                tool.callback,
                serde_json::json!({}),
                serde_json::json!({}),
                CallAuthority::Unscoped,
            ),
            Err(PluginError::StaleGeneration)
        ),
        "stale plugin generation reached its successor"
    );
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn transforms_and_hooks_retain_generation_bound_callbacks() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "callbacks",
        r#"
export default {
  id: "fixture.callbacks",
  async setup(ctx) {
    await ctx.agent.transform((draft) => {
      draft.update("agt_original", (agent) => { agent.name = "mutated" })
      draft.default("agt_original")
    })
    await ctx.tool.hook("execute.before", (event) => { event.input.value = 2 })
    const disposed = await ctx.session.hook("context", (event) => { event.hidden = true })
    await disposed.dispose()
  }
}
"#,
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let generation = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}))?;
    let registration = generation
        .registrations()
        .first()
        .ok_or("registration is absent")?;
    assert_eq!(registration.callbacks.len(), 2);
    assert!(registration
        .callbacks
        .iter()
        .all(|callback| callback.callback > 0));
    assert!(!registration
        .callbacks
        .iter()
        .any(|callback| callback.domain == "session"));

    let before = registration
        .callbacks
        .iter()
        .find(|callback| callback.domain == "tool" && callback.name == "hook.execute.before")
        .ok_or("tool before hook is absent")?;
    let event = supervisor.invoke_callback(
        generation.id(),
        &registration.plugin_id,
        registration
            .callbacks
            .iter()
            .find(|callback| callback.domain == "agent" && callback.name == "transform")
            .ok_or("agent transform is absent")?
            .callback,
        serde_json::json!({"items":[{"id":"agt_original","name":"original"}],"default":null}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(event["items"][0]["name"], "mutated");
    assert_eq!(event["default"], "agt_original");

    let event = supervisor.invoke_callback(
        generation.id(),
        &registration.plugin_id,
        before.callback,
        serde_json::json!({"input":{"value":1}}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(event["input"]["value"], 2);

    let wrong_owner = supervisor.invoke_callback(
        generation.id(),
        "fixture.other",
        before.callback,
        serde_json::json!({}),
        CallAuthority::Unscoped,
    );
    assert!(
        matches!(wrong_owner, Err(PluginError::Invocation(message)) if message.contains("not part of this generation"))
    );
    let stale = PluginGenerationId::new(generation.id().value() + 1).ok_or("generation")?;
    assert!(matches!(
        supervisor.invoke_callback(
            stale,
            &registration.plugin_id,
            before.callback,
            serde_json::json!({}),
            CallAuthority::Unscoped,
        ),
        Err(PluginError::StaleGeneration)
    ));
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn ratified_aisdk_replacement_exclusions_are_attributed_and_fail_closed(
) -> Result<(), Box<dyn Error>> {
    let cases = [
        (
            "sdk",
            r#"
export default {
  id: "fixture.aisdk-sdk",
  async setup(ctx) {
    await ctx.aisdk.hook("sdk", (event) => { event.sdk = "replacement" })
  }
}
"#,
            "fixture.aisdk-sdk",
            "aisdk.hook.sdk",
        ),
        (
            "language",
            r#"
export default {
  id: "fixture.aisdk-language",
  async setup(ctx) {
    await ctx.aisdk.hook("language", (event) => { event.model = "replacement" })
  }
}
"#,
            "fixture.aisdk-language",
            "aisdk.hook.language",
        ),
    ];
    for (label, source, plugin_id, surface) in cases {
        let (root, plugin) = fixture(label, source)?;
        let (runtime, _root_cleanup) = runtime()?;
        let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
        let activation = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}));
        let error = match activation {
            Ok(_) => panic!("{surface} replacement unexpectedly activated"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains(plugin_id)
                && error.contains(surface)
                && error.contains("grapt.plugin-aisdk-replacement-exclusion-2026-09-16")
                && error.contains("compiled provider composition"),
            "{surface} exclusion reached the wrong diagnostic: {error}"
        );
        assert!(
            supervisor.current()?.is_none(),
            "{surface} replacement published a generation after refusal"
        );
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn declared_transform_domains_receive_compatible_mutable_drafts() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "transform-drafts",
        r#"
export default {
  id: "fixture.drafts",
  async setup(ctx) {
    await ctx.agent.transform((draft) => {
      draft.update("agent-a", (agent) => { agent.name = "updated-agent" })
      draft.remove("agent-b")
      draft.default("agent-a")
    })
    await ctx.catalog.transform((draft) => {
      draft.provider.update("provider-a", (provider) => { provider.name = "updated-provider" })
      draft.model.update("provider-a", "model-a", (model) => { model.name = "updated-model" })
      draft.model.remove("provider-a", "model-b")
      draft.model.default.set("provider-a", "model-a")
    })
    await ctx.command.transform((draft) => {
      draft.update("build", (command) => { command.description = "updated-command" })
      draft.remove("remove")
    })
    await ctx.integration.transform((draft) => {
      draft.update("integration-a", (integration) => { integration.name = "updated-integration" })
      draft.method.remove("integration-a", {id:"old"})
      draft.method.update({integrationID:"integration-a", method:{id:"env",type:"env",names:["TOKEN"]}})
    })
    await ctx.reference.transform((draft) => {
      draft.remove("old")
      draft.add("new", {type:"local",path:"/reference"})
    })
    await ctx.skill.transform((draft) => draft.source({type:"local",path:"/skill"}))
    await ctx.websearch.transform((draft) => {
      draft.add({id:"fixture-search",name:"Fixture Search",async execute(input) { return [{title:input.query,url:"https://example.test"}] }})
      draft.default.set("fixture-search")
    })
  }
}
"#,
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(
        runtime,
        BridgeLimits {
            call_millis: 2_000,
            ..DEFAULT_BRIDGE_LIMITS
        },
    )?;
    let generation = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}))?;
    let registration = generation.registrations().first().ok_or("registration")?;
    let transform = |domain: &str| {
        registration
            .callbacks
            .iter()
            .find(|callback| callback.domain == domain && callback.name == "transform")
            .map(|callback| callback.callback)
            .ok_or_else(|| PluginError::Invocation(format!("{domain} transform is absent")))
    };
    let invoke = |domain: &str, value: serde_json::Value| {
        supervisor.invoke_callback(
            generation.id(),
            &registration.plugin_id,
            transform(domain)?,
            value,
            CallAuthority::Unscoped,
        )
    };

    let agent = invoke(
        "agent",
        serde_json::json!({"items":[{"id":"agent-a","name":"a"},{"id":"agent-b","name":"b"}],"default":null}),
    )?;
    assert_eq!(
        agent["items"],
        serde_json::json!([{"id":"agent-a","name":"updated-agent"}])
    );
    assert_eq!(agent["default"], "agent-a");

    let catalog = invoke(
        "catalog",
        serde_json::json!({"providers":[{"provider":{"id":"provider-a","name":"a"},"models":[{"id":"model-a","name":"a"},{"id":"model-b","name":"b"}]}],"default":null}),
    )?;
    assert_eq!(
        catalog["providers"][0]["provider"]["name"],
        "updated-provider"
    );
    assert_eq!(
        catalog["providers"][0]["models"],
        serde_json::json!([{"id":"model-a","name":"updated-model"}])
    );
    assert_eq!(
        catalog["default"],
        serde_json::json!({"providerID":"provider-a","modelID":"model-a"})
    );

    let command = invoke(
        "command",
        serde_json::json!({"items":[{"name":"build","description":"old"},{"name":"remove"}]}),
    )?;
    assert_eq!(
        command["items"],
        serde_json::json!([{"name":"build","description":"updated-command"}])
    );

    let integration = invoke(
        "integration",
        serde_json::json!({"items":[{"id":"integration-a","name":"a"}],"methods":[{"integrationID":"integration-a","method":{"id":"old","type":"key"}}]}),
    )?;
    assert_eq!(integration["items"][0]["name"], "updated-integration");
    assert_eq!(integration["methods"][0]["method"]["id"], "env");

    let reference = invoke(
        "reference",
        serde_json::json!({"items":[["old",{"type":"local","path":"/old"}]]}),
    )?;
    assert_eq!(
        reference["items"],
        serde_json::json!([["new",{"type":"local","path":"/reference"}]])
    );
    let skill = invoke("skill", serde_json::json!({"sources":[]}))?;
    assert_eq!(
        skill["sources"],
        serde_json::json!([{"type":"local","path":"/skill"}])
    );
    let websearch = invoke(
        "websearch",
        serde_json::json!({"providers":[],"default":null}),
    )?;
    assert_eq!(websearch["providers"][0]["id"], "fixture-search");
    assert_eq!(websearch["default"], "fixture-search");
    let execute = websearch["providers"][0]["callback"]
        .as_u64()
        .ok_or("websearch callback identity")?;
    let results = supervisor.invoke_callback(
        generation.id(),
        &registration.plugin_id,
        execute,
        serde_json::json!({"query":"query"}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(results[0]["title"], "query");

    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[derive(Debug, Default)]
struct FixtureHost {
    calls: Mutex<Vec<HostOperation>>,
}

impl PluginHost for FixtureHost {
    fn call(
        &self,
        _context: &PluginHostContext,
        operation: HostOperation,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.calls
            .lock()
            .map_err(|_| "fixture host calls are poisoned".to_owned())?
            .push(operation);
        match operation {
            HostOperation::AgentList => Ok(serde_json::json!({
                "location": {"directory":"fixture"},
                "data": [{"id":"agt_fixture"}],
            })),
            HostOperation::SessionGet => Ok(serde_json::json!({
                "id": payload.get("sessionID").cloned().unwrap_or_default(),
            })),
            _ => Err(format!("unexpected fixture operation {operation:?}")),
        }
    }
}

#[test]
fn reverse_host_operations_are_typed_bounded_and_available_during_calls(
) -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "host-operations",
        r#"
export default {
  id: "fixture.host",
  async setup(ctx) {
    const agents = await ctx.agent.list({})
    if (agents.data[0].id !== "agt_fixture") throw new Error("agent owner response was lost")
    await ctx.tool.transform((draft) => draft.add({
      name: "fixture_host",
      args: {type:"object",additionalProperties:false},
      async execute(_input, context) {
        const session = await ctx.session.get({sessionID:context.sessionID})
        return {title:"host",output:session.id,metadata:{}}
      }
    }))
  }
}
"#,
    )?;
    let host = Arc::new(FixtureHost::default());
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::with_host(
        runtime,
        DEFAULT_BRIDGE_LIMITS,
        Arc::clone(&host) as Arc<dyn PluginHost>,
    )?;
    let generation = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}))?;
    let registration = generation.registrations().first().ok_or("registration")?;
    let tool = registration.tools.first().ok_or("tool")?;
    let output = supervisor.invoke(
        generation.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({}),
        serde_json::json!({"sessionID":"ses_host"}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(output["output"], "ses_host");
    assert_eq!(
        *host.calls.lock().map_err(|_| "fixture calls")?,
        vec![HostOperation::AgentList, HostOperation::SessionGet]
    );
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[path = "bridge/access.rs"]
mod access;
#[path = "bridge/containment.rs"]
mod containment;

#[path = "bridge/isolation.rs"]
mod isolation;
#[path = "bridge/lifecycle.rs"]
mod lifecycle;
#[cfg(target_os = "linux")]
#[path = "bridge/reap.rs"]
mod reap;
#[path = "bridge/resource.rs"]
mod resource;
#[path = "bridge/transport.rs"]
mod transport;
#[cfg(target_os = "linux")]
#[path = "bridge/unit_identity.rs"]
mod unit_identity;

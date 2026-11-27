use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use grapt_store::{apply_migrations, EventStore, MigrationOutcome, ProductionMigrations};
use sha2::{Digest, Sha256};
use tokio::time::Instant;

#[path = "plugin_compatibility/evidence.rs"]
mod evidence;

use evidence::product_snapshot;

/// Pipe-drain bounds. The plugin host is a separate process that can hold the
/// product's pipes open after the product itself is killed, so an unbounded
/// drain could hang.
const EVIDENCE_PIPE_BYTES: u64 = 16 * 1_024;
const EVIDENCE_PIPE_WAIT: Duration = Duration::from_secs(2);

/// Reap bounds for a failed product: every wait is bounded in time.
const EVIDENCE_REAP_POLLS: usize = 50;
const EVIDENCE_REAP_SLEEP: Duration = Duration::from_millis(100);

/// A launched product the test has not yet settled. Every way out of the test
/// while it is held (a failed step, an early `?` or a failed assertion) goes
/// through `failure`, so the product's evidence is kept and the product is
/// killed and reaped rather than left behind.
struct Product {
    child: Option<Child>,
    root: PathBuf,
    label: &'static str,
    launched: Instant,
    stage: &'static str,
}

impl Product {
    fn launch(
        label: &'static str,
        root: &Path,
        explicit: &Path,
        node: &Path,
        bridge: &Path,
        runtime_manifest: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        let launched = Instant::now();
        let child = launch(root, explicit, node, bridge, runtime_manifest)?;
        Ok(Self {
            child: Some(child),
            root: root.to_path_buf(),
            label,
            launched,
            stage: "launch",
        })
    }

    /// One step after launch. Its error keeps the product's evidence, and the
    /// stage is remembered for a later assertion that fails.
    fn step<T, E: std::fmt::Display>(
        &mut self,
        stage: &'static str,
        step: impl FnOnce(&mut Child) -> Result<T, E>,
    ) -> Result<T, Box<dyn Error>> {
        self.stage = stage;
        let Some(child) = self.child.as_mut() else {
            return Err(format!("[{} product, {stage}] already settled", self.label).into());
        };
        match step(&mut *child) {
            Ok(value) => Ok(value),
            Err(error) => {
                let evidence = failure(child, &self.root, self.label, stage, self.launched, error);
                // The evidence is taken and the product reaped. Releasing it here
                // keeps the drop path from recording a second, empty snapshot.
                self.child = None;
                Err(evidence)
            }
        }
    }

    /// Hands the product back once the test no longer needs its evidence.
    fn settle(mut self) -> Result<Child, Box<dyn Error>> {
        self.child
            .take()
            .ok_or_else(|| format!("[{} product] already settled", self.label).into())
    }

    /// End the fixture through the product's existing terminal control pipe,
    /// then prove clean shutdown rather than waiting out its fallback window.
    fn stop(&mut self) -> Result<(), Box<dyn Error>> {
        self.step("close terminal control", |child| {
            child
                .stdin
                .take()
                .map(drop)
                .ok_or("product terminal control pipe is absent")
        })?;
        self.step(
            "wait for clean terminal shutdown",
            |child| -> Result<(), Box<dyn Error>> {
                let status = wait_for_exit(child, Duration::from_secs(30))?;
                if status.success() {
                    Ok(())
                } else {
                    Err(format!("product exited unsuccessfully: {status}").into())
                }
            },
        )
    }
}

impl Drop for Product {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let ending = if thread::panicking() {
                "an assertion failed after this stage"
            } else {
                "the test returned early after this stage"
            };
            eprintln!(
                "{}",
                failure(
                    child,
                    &self.root,
                    self.label,
                    self.stage,
                    self.launched,
                    ending
                )
            );
        }
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!(
            "secode-plugin-product-{}-{label}",
            std::process::id()
        ));
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn node() -> Result<PathBuf, Box<dyn Error>> {
    let candidate = if let Some(path) = std::env::var_os("NODE") {
        PathBuf::from(path)
    } else {
        let (program, argument) = if cfg!(windows) {
            ("where.exe", "node.exe")
        } else {
            ("which", "node")
        };
        let text = node_output(Command::new(program).arg(argument))?;
        PathBuf::from(text.lines().next().ok_or("Node lookup returned no path")?)
    };
    // PATH may name a version-manager launcher. Pin and execute the actual
    // runtime, as the plugin bridge fixtures already do, not that launcher.
    let resolved = node_output(
        Command::new(candidate)
            .arg("-e")
            .arg("process.stdout.write(process.execPath)"),
    )?;
    let executable = PathBuf::from(resolved.trim());
    if !executable.is_absolute() || !executable.is_file() {
        return Err("Node did not resolve to an absolute executable file".into());
    }
    let version = node_output(Command::new(&executable).arg("--version"))?;
    if !version.trim().starts_with("v24.") {
        return Err(format!(
            "plugin tests require Node 24, found {}; install Node 24 and set NODE to its executable",
            version.trim().escape_debug()
        )
        .into());
    }
    Ok(executable)
}

fn wait_for_exit(child: &mut Child, bound: Duration) -> Result<ExitStatus, Box<dyn Error>> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= bound {
            return Err(format!("child did not exit within {}ms", bound.as_millis()).into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Node discovery is setup work, not plugin execution. Bound both its process
/// lifetime and output so a launcher cannot hang the evidence test.
fn node_output(command: &mut Command) -> Result<String, Box<dyn Error>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let output = child.stdout.take().ok_or("Node probe has no stdout")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("plugin-node-probe".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = output.take(4097).read_to_end(&mut bytes).map(|_| bytes);
            let _ = sender.send(result);
        });
    if let Err(error) = reader {
        child.kill()?;
        let _status = wait_for_exit(&mut child, Duration::from_secs(5))?;
        return Err(error.into());
    }
    let status = match wait_for_exit(&mut child, Duration::from_secs(10)) {
        Ok(status) => status,
        Err(error) => {
            child.kill()?;
            let _status = wait_for_exit(&mut child, Duration::from_secs(5))?;
            return Err(error);
        }
    };
    if !status.success() {
        return Err(format!("Node probe exited unsuccessfully: {status}").into());
    }
    let bytes = receiver.recv_timeout(Duration::from_secs(1))??;
    if bytes.len() > 4096 {
        return Err("Node probe output exceeds 4096 bytes".into());
    }
    Ok(String::from_utf8(bytes)?)
}

fn sha256(path: &Path) -> Result<String, Box<dyn Error>> {
    let digest = Sha256::digest(fs::read(path)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn runtime_manifest(
    root: &Path,
    executable: &Path,
    bridge: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let version = node_output(Command::new(executable).arg("--version"))?
        .trim()
        .to_owned();
    let executable_sha256 = sha256(executable)?;
    let target = if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let path = root.join("runtime.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "grapt.plugin-runtime.v1",
            "target": target,
            "version": version,
            "executable_sha256": executable_sha256,
            "bridge_sha256": sha256(bridge)?,
            "source_url": format!("https://nodejs.org/dist/{version}/integration-fixture"),
            "source_sha256": executable_sha256,
            "license_sha256": executable_sha256,
        }))?,
    )?;
    Ok(path)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn write_service(root: &Path) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    // The explicit plugin is user-owned; the separate .grapt declaration
    // below remains project-owned and must still be blocked.
    fs::write(
        root.join("service.conf"),
        "slot=product\ninstance=plugin-fixture\nenvironment=internal-alpha\nversion=0.1.0\nserve_window_ms=120000\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=60000\nprovider_active_attempts=4\n",
    )?;
    fs::write(root.join("basic.credential"), "alpha:zephyr")?;
    fs::write(root.join("provider.credential"), "provider-fixture")?;
    fs::write(
        root.join("account-models.json"),
        r#"{"models":[{"slug":"gpt-5.6","visibility":"list","context_window":200000,"supported_reasoning_levels":[]}]}"#,
    )?;
    fs::write(
        root.join("model-catalog.json"),
        r#"{"openai":{"id":"openai","name":"OpenAI","models":{"gpt-5.6":{"id":"gpt-5.6","name":"gpt-5.6","family":"gpt-5","tool_call":true,"modalities":{"input":["text"],"output":["text"]},"limit":{"context":200000,"output":32000}}}}}"#,
    )?;
    let project_plugin = root.join("project-plugin.mjs");
    fs::write(
        &project_plugin,
        "export default {id:'project.blocked',setup(){}}\n",
    )?;
    fs::write(
        root.join("grapt.jsonc"),
        r#"{
  "store": { "authority": "open-ready" },
  "product": { "build": "plugin-build" },
  "catalog": { "source": "models.dev-chatgpt" },
  "provider": {
    "id": "openai", "model": "gpt-5.6", "base_model": "gpt-5.6",
    "family": "gpt-5", "route": "responses", "account": "account-a",
    "credential_version": "credential-v1", "policy_version": "df0-policy-v1", "variants": []
  }
}"#,
    )?;
    // The profile's own grapt.jsonc is the user's, so a repository's
    // plugin is declared where a repository keeps it: its .grapt.
    fs::create_dir_all(root.join(".grapt"))?;
    fs::write(
        root.join(".grapt").join("grapt.json"),
        format!(
            r#"{{"plugins": ["{}"]}}"#,
            project_plugin.to_string_lossy().replace('\\', "\\\\")
        ),
    )?;
    let cleanup = root.join("plugin-cleanup.txt");
    let event = root.join("plugin-event.json");
    let reference = root.join("plugin-reference");
    fs::create_dir_all(&reference)?;
    let skill = root.join("plugin-skill.md");
    fs::write(&skill, "# Plugin skill\n")?;
    let external = root.join("external-plugin.mjs");
    fs::write(
        &external,
        r#"import { writeFile } from "node:fs/promises"
export default {
  id: "fixture.external",
  async setup(ctx) {
    const agents = await ctx.agent.list({})
    const agent = await ctx.agent.get({agentID:"agt_live_headless1"})
    const providers = await ctx.catalog.provider.list({})
    const provider = await ctx.catalog.provider.get({providerID:"openai"})
    const models = await ctx.catalog.model.list({})
    const defaultModel = await ctx.catalog.model.default({})
    const commands = await ctx.command.list({})
    const integrations = await ctx.integration.list({})
    const plugins = await ctx.plugin.list({})
    const references = await ctx.reference.list({})
    const skills = await ctx.skill.list({})
    if (!agents.data.some((agent) => agent.id === "agt_live_headless1")) throw new Error("agent owner response is absent")
    if (agent.data.id !== "agt_live_headless1") throw new Error("agent get owner response is absent")
    if (!providers.data.some((provider) => provider.id === "openai")) throw new Error("provider owner response is absent")
    if (provider.data.id !== "openai" || !Array.isArray(models.data) || !defaultModel.data) throw new Error("catalog owner response is absent")
    if (!Array.isArray(commands.data) || !Array.isArray(integrations.data) || !Array.isArray(plugins.data) || !Array.isArray(references.data) || !Array.isArray(skills.data)) throw new Error("product catalog response is absent")
    // Session and event authority exists only inside a host-granted call.
    const refusals = []
    for (const attempt of [
      () => ctx.session.create({}),
      () => ctx.event.subscribe()[Symbol.asyncIterator]().next(),
      () => ctx.session.get({sessionID: "ses_fixtureforeign"}),
    ]) {
      try { await attempt(); refusals.push("allowed") } catch (error) { refusals.push(String(error?.message ?? error)) }
    }
    await writeFile(ctx.options.event, JSON.stringify(refusals), "utf8")
    await Promise.all([ctx.agent.reload(),ctx.catalog.reload(),ctx.command.reload(),ctx.integration.reload(),ctx.reference.reload(),ctx.skill.reload()])
    await ctx.agent.transform((draft) => draft.update("agt_live_headless1", (agent) => { agent.name = "Plugin Mutated Agent" }))
    await ctx.catalog.transform((draft) => {
      draft.provider.update("openai", (provider) => { provider.name = "Plugin Mutated Provider" })
      draft.model.update("openai", "gpt-5.6", (model) => { model.name = "Plugin Mutated Model" })
      draft.model.default.set("openai", "gpt-5.6")
    })
    await ctx.command.transform((draft) => draft.update("init", (command) => { command.description = "Plugin Mutated Command" }))
    await ctx.integration.transform((draft) => draft.update("openai", (integration) => { integration.name = "Plugin Mutated Integration" }))
    await ctx.reference.transform((draft) => draft.add("plugin-reference", {type:"local",path:ctx.options.reference,description:"Plugin Reference"}))
    await ctx.skill.transform((draft) => draft.source({type:"embedded",skill:{id:"plugin-skill",name:"Plugin Skill",description:"Plugin Skill Source",location:ctx.options.skill,content:"Plugin skill"}}))
    await ctx.tool.transform((draft) => draft.add({
      name: "fixture_echo",
      description: "external compatibility witness",
      args: {type:"object",additionalProperties:false,properties:{text:{type:"string"}},required:["text"]},
      async execute(input) { return {title:"fixture",output:input.text,metadata:{}} }
    }))
    return async () => writeFile(ctx.options.cleanup, "cleaned", "utf8")
  }
}
"#,
    )?;
    let explicit = root.join("explicit-plugin.jsonc");
    fs::write(
        &explicit,
        serde_json::to_vec_pretty(&serde_json::json!({
            "plugins": [{
                "package": external.to_string_lossy(),
                "options": {
                    "cleanup": cleanup.to_string_lossy(),
                    "event": event.to_string_lossy(),
                    "reference": reference.to_string_lossy(),
                    "skill": skill.to_string_lossy()
                }
            }]
        }))?,
    )?;
    Ok((explicit, cleanup))
}

fn create_store(root: &Path) -> Result<(), Box<dyn Error>> {
    let database = root.join("database.sqlite3");
    drop(EventStore::open(&database, "plugin-product-test")?);
    let migrations = ProductionMigrations::load()?;
    match apply_migrations(&database, root, "0.1.0", &migrations.as_slice())? {
        MigrationOutcome::Applied { .. } | MigrationOutcome::AlreadyCurrent => Ok(()),
    }
}

fn request(address: SocketAddr, path: &str) -> Result<(u16, serde_json::Value), Box<dyn Error>> {
    // Each phase's error names itself and how long the request had run, so a
    // refused connect, a slow response and an empty response are distinguishable.
    let started = Instant::now();
    let mut stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(2)).map_err(|error| {
            format!(
                "plugin GET {path} could not connect after {}ms: {error}",
                started.elapsed().as_millis()
            )
        })?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let token = base64::engine::general_purpose::STANDARD.encode("alpha:zephyr");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic {token}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| {
        format!(
            "write {path} failed after {}ms: {error}",
            started.elapsed().as_millis()
        )
    })?;
    let mut bytes = Vec::new();
    stream
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            format!(
                "plugin GET {path} response read after {} bytes and {}ms: {error}",
                bytes.len(),
                started.elapsed().as_millis()
            )
        })?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(format!("plugin GET {path} exceeded its response byte bound").into());
    }
    let received = bytes.len();
    let text = String::from_utf8(bytes)?;
    let (head, body) = text.split_once("\r\n\r\n").ok_or_else(|| {
        format!(
            "plugin GET {path} closed without a response head ({received} bytes) after {}ms",
            started.elapsed().as_millis()
        )
    })?;
    let status = head
        .split_whitespace()
        .nth(1)
        .ok_or("HTTP status")?
        .parse::<u16>()?;
    Ok((status, serde_json::from_str(body)?))
}

fn launch(
    root: &Path,
    explicit: &Path,
    node: &Path,
    bridge: &Path,
    runtime_manifest: &Path,
) -> Result<Child, Box<dyn Error>> {
    Ok(
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .args(["serve-terminal", "--service"])
            .arg(root)
            .args(["--port", "0"])
            .env("GRAPT_TEST_HOME", root.join("home"))
            .env("GRAPT_CONFIG_DIR", root.join("global-config"))
            .env("GRAPT_CONFIG", explicit)
            .env("GRAPT_PLUGIN_NODE", node)
            .env("GRAPT_PLUGIN_BRIDGE", bridge)
            .env("GRAPT_PLUGIN_RUNTIME_MANIFEST", runtime_manifest)
            .env("GRAPT_LIVE_HEADLESS_LOOPBACK", "http://127.0.0.1:9")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    )
}

fn ready(child: &mut Child, root: &Path) -> Result<SocketAddr, Box<dyn Error>> {
    let ready_path = root.join("serve-ready.json");
    for _ in 0..3000 {
        if let Ok(bytes) = fs::read(&ready_path) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                return value["observed_endpoint"]
                    .as_str()
                    .and_then(|endpoint| endpoint.strip_prefix("http://"))
                    .ok_or_else(|| "ready endpoint is absent".into())
                    .and_then(|endpoint| endpoint.parse().map_err(Into::into));
            }
        }
        if child.try_wait()?.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err("product failed before publishing its ready receipt".into())
}

/// Reads at most `EVIDENCE_PIPE_BYTES` from a pipe, and waits at most
/// `EVIDENCE_PIPE_WAIT` for it. A reader still blocked after that is left
/// behind rather than joined.
fn drain(pipe: Option<impl Read + Send + 'static>) -> String {
    let Some(pipe) = pipe else {
        return "absent".to_owned();
    };
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = pipe
            .take(EVIDENCE_PIPE_BYTES)
            .read_to_end(&mut bytes)
            .map(|_| ());
        let _ = sender.send((read, bytes));
    });
    match receiver.recv_timeout(EVIDENCE_PIPE_WAIT) {
        Ok((Ok(()), bytes)) => {
            format!("{}B:{}", bytes.len(), String::from_utf8_lossy(&bytes))
        }
        Ok((Err(error), bytes)) => format!(
            "read error {error} after {}B:{}",
            bytes.len(),
            String::from_utf8_lossy(&bytes)
        ),
        Err(_) => format!(
            "not drained within {}ms (pipe held open)",
            EVIDENCE_PIPE_WAIT.as_millis()
        ),
    }
}

/// Turns a post-launch failure into one that keeps the product's evidence. The
/// snapshot comes first, because `try_wait` reaps an exited product and its
/// `/proc` entry goes with it. Then the product is killed, reaped within
/// `EVIDENCE_REAP_POLLS * EVIDENCE_REAP_SLEEP`, and its pipes drained with bounds.
fn failure(
    child: &mut Child,
    root: &Path,
    product: &str,
    stage: &str,
    launched: Instant,
    error: impl std::fmt::Display,
) -> Box<dyn Error> {
    let elapsed_ms = launched.elapsed().as_millis();
    let snapshot = product_snapshot(child.id(), Some(&root.join("plugin-runtime")));
    let before = child.try_wait();
    let _ = child.kill();
    let mut reaped = None;
    for _ in 0..EVIDENCE_REAP_POLLS {
        match child.try_wait() {
            Ok(Some(status)) => {
                reaped = Some(Ok(status));
                break;
            }
            Ok(None) => thread::sleep(EVIDENCE_REAP_SLEEP),
            Err(error) => {
                reaped = Some(Err(error));
                break;
            }
        }
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    format!(
        "[{product} product, {stage}] {error}; elapsed_since_launch_ms={elapsed_ms}; \
         status_before_kill={before:?}; {snapshot}; reaped={reaped:?}; \
         stdout={stdout}; stderr={stderr}"
    )
    .into()
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn explicit_plugin_registers_and_project_code_is_blocked_then_cleanup_runs(
) -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new("active")?;
    create_store(&scratch.0)?;
    let (explicit, cleanup) = write_service(&scratch.0)?;
    let bridge =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/plugin-bridge/index.mjs");
    let node = node()?;
    let runtime_manifest = runtime_manifest(&scratch.0, &node, &bridge)?;
    let mut product = Product::launch(
        "active",
        &scratch.0,
        &explicit,
        &node,
        &bridge,
        &runtime_manifest,
    )?;
    let address = product.step("ready", |child| ready(child, &scratch.0))?;
    let (status, body) =
        product.step("request /api/plugin", |_| request(address, "/api/plugin"))?;
    assert_eq!(status, 200, "plugin status request failed: {body}");
    let rows = body["data"].as_array().ok_or("plugin rows")?;
    assert!(rows.iter().any(|row| {
        row["id"] == "fixture.external" && row["status"] == "active" && row["generation"] == 1
    }));
    assert!(rows.iter().any(|row| {
        row["id"]
            .as_str()
            .is_some_and(|id| id.ends_with("project-plugin.mjs"))
            && row["status"] == "blocked"
            && row["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("launch-explicit"))
    }));
    let (agent_status, agents) =
        product.step("request /api/agent", |_| request(address, "/api/agent"))?;
    assert_eq!(agent_status, 200, "agent request failed: {agents}");
    assert!(agents["data"]
        .as_array()
        .is_some_and(|rows| rows
            .iter()
            .any(|agent| agent["id"] == "agt_live_headless1"
                && agent["name"] == "Plugin Mutated Agent")));
    let (command_status, commands) =
        product.step("request /api/command", |_| request(address, "/api/command"))?;
    assert_eq!(command_status, 200, "command request failed: {commands}");
    assert!(commands["data"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|command| command["name"] == "init"
            && command["description"] == "guided AGENTS.md setup")));
    let (status, inspected) = product.step("inspect protected command provenance", |_| {
        request(address, "/api/command/inspect")
    })?;
    assert_eq!(status, 200, "command inspection failed: {inspected}");
    assert!(inspected["data"]
        .as_array()
        .is_some_and(
            |rows| rows.iter().any(|row| row["command"]["name"] == "init"
                && row["origin"] == "builtin"
                && row["source"] == "Grapt built-in"
                && row["command"]["description"] == "guided AGENTS.md setup")
        ));
    let (provider_status, providers) = product.step("request /api/provider", |_| {
        request(address, "/api/provider")
    })?;
    assert_eq!(provider_status, 200, "provider request failed: {providers}");
    assert!(providers["data"].as_array().is_some_and(|rows| rows
        .iter()
        .any(
            |provider| provider["id"] == "openai" && provider["name"] == "Plugin Mutated Provider"
        )));
    let (model_status, models) =
        product.step("request /api/model", |_| request(address, "/api/model"))?;
    assert_eq!(model_status, 200, "model request failed: {models}");
    assert!(models["data"].as_array().is_some_and(|rows| rows
        .iter()
        .any(|model| model["id"] == "gpt-5.6" && model["name"] == "Plugin Mutated Model")));
    let (default_status, default_model) = product.step("request /api/model/default", |_| {
        request(address, "/api/model/default")
    })?;
    assert_eq!(
        default_status, 200,
        "default model request failed: {default_model}"
    );
    assert_eq!(default_model["data"]["name"], "Plugin Mutated Model");
    let (integration_status, integrations) = product.step("request /api/integration", |_| {
        request(address, "/api/integration")
    })?;
    assert_eq!(
        integration_status, 200,
        "integration request failed: {integrations}"
    );
    assert!(integrations["data"].as_array().is_some_and(|rows| rows
        .iter()
        .any(|integration| integration["id"] == "openai"
            && integration["name"] == "Plugin Mutated Integration")));
    let (reference_status, references) = product.step("request /api/reference", |_| {
        request(address, "/api/reference")
    })?;
    assert_eq!(
        reference_status, 200,
        "reference request failed: {references}"
    );
    assert!(references["data"].as_array().is_some_and(|rows| rows
        .iter()
        .any(|reference| reference["name"] == "plugin-reference"
            && reference["description"] == "Plugin Reference")));
    let (skill_status, skills) =
        product.step("request /api/skill", |_| request(address, "/api/skill"))?;
    assert_eq!(skill_status, 200, "skill request failed: {skills}");
    assert!(skills["data"].as_array().is_some_and(|rows| rows
        .iter()
        .any(|skill| skill["id"] == "plugin-skill" && skill["name"] == "Plugin Skill")));
    let refusals: Vec<String> = product.step("read plugin-event.json", |_| {
        fs::read(scratch.0.join("plugin-event.json"))
            .map_err(|error| -> Box<dyn Error> { error.into() })
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
    })?;
    assert_eq!(refusals.len(), 3);
    assert!(
        refusals
            .iter()
            .all(|refusal| refusal.contains("requires an active plugin call")),
        "setup-time Session or event authority was not refused for the right reason: {refusals:?}"
    );
    product.stop()?;
    assert!(
        cleanup.is_file(),
        "plugin cleanup is absent after clean product shutdown"
    );
    assert_eq!(fs::read_to_string(cleanup)?, "cleaned");
    product.settle()?;
    Ok(())
}

#[test]
fn explicit_disable_survives_product_restart_without_an_active_generation(
) -> Result<(), Box<dyn Error>> {
    let scratch = Scratch::new("disable-restart")?;
    create_store(&scratch.0)?;
    let (explicit, cleanup) = write_service(&scratch.0)?;
    let bridge =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/plugin-bridge/index.mjs");
    let node = node()?;
    let runtime_manifest = runtime_manifest(&scratch.0, &node, &bridge)?;

    let mut first = Product::launch(
        "first",
        &scratch.0,
        &explicit,
        &node,
        &bridge,
        &runtime_manifest,
    )?;
    let first_address = first.step("ready", |child| ready(child, &scratch.0))?;
    let (_, first_body) = first.step("request /api/plugin", |_| {
        request(first_address, "/api/plugin")
    })?;
    assert!(first_body["data"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["status"] == "active")));
    first.stop()?;
    assert_eq!(fs::read_to_string(&cleanup)?, "cleaned");
    // The first product has exited and its cleanup is proven.
    first.settle()?;

    let external = scratch.0.join("external-plugin.mjs");
    fs::write(
        &explicit,
        serde_json::to_vec_pretty(&serde_json::json!({
            "plugins": [format!("-{}", external.to_string_lossy())]
        }))?,
    )?;
    let ready_receipt = scratch.0.join("serve-ready.json");
    if ready_receipt.is_file() {
        fs::remove_file(ready_receipt)?;
    }
    fs::remove_file(&cleanup)?;

    let mut restarted = Product::launch(
        "restarted",
        &scratch.0,
        &explicit,
        &node,
        &bridge,
        &runtime_manifest,
    )?;
    let restarted_address = restarted.step("ready", |child| ready(child, &scratch.0))?;
    let (status, body) = restarted.step("request /api/plugin", |_| {
        request(restarted_address, "/api/plugin")
    })?;
    assert_eq!(status, 200);
    assert!(body["data"]
        .as_array()
        .is_some_and(|rows| rows.iter().all(|row| row["status"] != "active")));
    restarted.stop()?;
    assert!(!cleanup.exists());
    restarted.settle()?;
    Ok(())
}

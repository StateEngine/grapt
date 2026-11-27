//! The product-envelope witness (PLR-R10): the bridge test binary gates
//! unrelated fixture activations to a measured-green width, but the
//! product envelope itself is witnessed here, in its own binary, where
//! cargo runs it separately from the bridge binary and the gate never
//! applies.
//!
//! The envelope is derived from the product, not chosen (2026-09-27,
//! Rust tree at the R9 docs sha): `DEFAULT_SUPERVISOR_BUDGET`
//! (protocol.rs) sets `processes = 16` and `startup_concurrency = 4`
//! per supervisor, and `ProcessBudget::reload_safe_plugins()` (budget.rs)
//! is `limit / 2`, so one generation may hold 8 plugins at the default
//! and a reload runs 8 beside 8 = the full 16-process budget. There is
//! no supervisor-count cap in the crate (rg-verified this round), so
//! multiple supervisors are permitted, each with its own budget and
//! containment slice. The per-exchange product bound is
//! `DEFAULT_BRIDGE_LIMITS.startup_millis = 10 s`; the supervisor bounds
//! every startup permit by its own thread start plus that allowance, so
//! a second wave's permit deadline starts at t = 0 - the exact P4
//! admission class the gate isolates from unrelated fixture pressure.
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use grapt_plugin::{
    resolve_declarations, BridgeRuntime, ConfigDocument, HostOperation, PluginError, PluginHost,
    PluginHostContext, PluginOrigin, PluginSupervisor, RuntimePin, DEFAULT_BRIDGE_LIMITS,
    DEFAULT_SUPERVISOR_BUDGET,
};
use sha2::{Digest, Sha256};
use tokio::time::Instant;

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
            return Err("Node is required for the plugin envelope witness".into());
        }
        let text = String::from_utf8(output.stdout)?;
        let first = text
            .lines()
            .next()
            .ok_or("Node lookup returned no executable")?;
        PathBuf::from(first)
    };
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

#[path = "support/recovery.rs"]
mod support;

struct RootCleanup(PathBuf);

impl Drop for RootCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(support::recovery_root(&self.0));
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "plugin envelope fixture could not clean up its private root {}: {error}",
                    self.0.display()
                );
            }
        }
    }
}

fn runtime() -> Result<(BridgeRuntime, RootCleanup), Box<dyn Error>> {
    static NEXT_RUNTIME: AtomicUsize = AtomicUsize::new(0);
    let runtime_id = NEXT_RUNTIME
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            (next < 4096).then_some(next + 1)
        })
        .map_err(|_| "plugin envelope fixture runtime limit exhausted")?;
    let executable = node()?;
    let version = String::from_utf8(Command::new(&executable).arg("--version").output()?.stdout)?
        .trim()
        .to_owned();
    let private_root = std::env::temp_dir().join(format!(
        "secode-plugin-envelope-runtime-{}-{runtime_id}",
        std::process::id()
    ));
    Ok((
        BridgeRuntime {
            expected: RuntimePin {
                version,
                executable_sha256: sha256(&executable)?,
                bridge_sha256: sha256(&bridge())?,
            },
            executable,
            bridge: bridge(),
            recovery: support::recovery(&private_root),
            private_root: private_root.clone(),
        },
        RootCleanup(private_root),
    ))
}

fn sha256(path: &Path) -> Result<String, Box<dyn Error>> {
    let digest = Sha256::digest(std::fs::read(path)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
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

struct NoopHost;

impl PluginHost for NoopHost {
    fn call(
        &self,
        _context: &PluginHostContext,
        _operation: HostOperation,
        _payload: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Err("the envelope witness performs no host calls".to_owned())
    }
}

#[test]
fn the_default_budget_envelope_admits_a_full_reload_and_refuses_one_over_the_reload_safe_bound(
) -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!("secode-plugin-envelope-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    let mut batch = Vec::new();
    for index in 0..8 {
        let path = root.join(format!("plugin-{index}")).join("plugin.mjs");
        std::fs::create_dir_all(path.parent().ok_or("plugin parent")?)?;
        std::fs::write(
            &path,
            format!("export default {{id:'fixture.envelope.{index}',setup(){{}}}}\n"),
        )?;
        batch.extend(declarations(&path)?);
    }
    assert_eq!(batch.len(), 8);
    let over_path = root.join("plugin-8").join("plugin.mjs");
    std::fs::create_dir_all(over_path.parent().ok_or("plugin parent")?)?;
    std::fs::write(
        &over_path,
        "export default {id:'fixture.envelope.over',setup(){}}\n",
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::with_budget(
        runtime,
        DEFAULT_BRIDGE_LIMITS,
        DEFAULT_SUPERVISOR_BUDGET,
        Arc::new(NoopHost),
    )?;
    let mut over = batch.clone();
    over.extend(declarations(&over_path)?);
    assert_eq!(over.len(), 9);
    let first_started = Instant::now();
    let first = supervisor.activate(batch.clone(), serde_json::json!({}))?;
    let first_elapsed = first_started.elapsed();
    assert_eq!(supervisor.live_processes(), 8);
    // The second full activation succeeds only if the budget admits 8
    // candidate processes beside the 8 live ones: 16 of 16, the
    // reload overlap at the full default budget. The per-exchange 10 s
    // bound does the failing the product owns; a breach reds this
    // activation as a Timeout, and no total-elapsed product bound is
    // asserted - the product promises per exchange, not in aggregate.
    let second_started = Instant::now();
    let second = supervisor.activate(batch, serde_json::json!({}))?;
    let second_elapsed = second_started.elapsed();
    assert_ne!(second.id(), first.id());
    assert_eq!(
        supervisor.live_processes(),
        8,
        "the retired generation was not reaped from the budget"
    );
    eprintln!(
        "envelope receipt: first activation {first_elapsed:?}, second (full-budget reload) {second_elapsed:?}"
    );
    assert!(
        supervisor.current_is_running(second.id())?,
        "the reload refused or disturbed the published generation"
    );
    let over = supervisor.prepare(over, serde_json::json!({}));
    assert!(
        matches!(&over, Err(PluginError::Registration(message)) if message.contains("exceed the reload-safe bound of 8")),
        "one plugin over the reload-safe bound at the default budget was not refused at admission: {over:?}"
    );
    assert!(
        supervisor.current_is_running(second.id())?,
        "the admission refusal disturbed the published generation"
    );
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// A bounded two-way rendezvous (the W2 law: a hung witness is a
/// harness defect): each side marks itself ready, then polls the
/// other's flag for at most `ticks` 100 ms ticks. A side whose partner
/// never arrives proceeds alone and reports it in the receipt, so an
/// environment failure completes or reds instead of hanging the
/// witness.
fn rendezvous(self_ready: &AtomicBool, partner: &AtomicBool, ticks: u32) -> bool {
    self_ready.store(true, Ordering::Release);
    for _ in 0..ticks {
        if partner.load(Ordering::Acquire) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

struct ExchangeReport {
    first: Duration,
    reload: Duration,
    rendezvous: bool,
}

/// One supervisor's full sequence: 8 plugins, the bounded rendezvous
/// so both supervisors' reloads overlap by construction, the
/// full-budget reload, the admission refusal over the bound, and the
/// shutdown. The receipt is per exchange; no aggregate bound is
/// asserted anywhere in this witness.
fn one_supervisor_exchange(
    batch: Vec<grapt_plugin::PluginDeclaration>,
    over: Vec<grapt_plugin::PluginDeclaration>,
    self_ready: Arc<AtomicBool>,
    partner: Arc<AtomicBool>,
) -> Result<ExchangeReport, String> {
    let (runtime, _root_cleanup) = runtime().map_err(|error| error.to_string())?;
    let supervisor = PluginSupervisor::with_budget(
        runtime,
        DEFAULT_BRIDGE_LIMITS,
        DEFAULT_SUPERVISOR_BUDGET,
        Arc::new(NoopHost),
    )
    .map_err(|error| error.to_string())?;
    let first_started = Instant::now();
    let first = supervisor
        .activate(batch.clone(), serde_json::json!({}))
        .map_err(|error| error.to_string())?;
    let first_elapsed = first_started.elapsed();
    assert_eq!(supervisor.live_processes(), 8);
    let overlapped = rendezvous(&self_ready, &partner, 1_200);
    let reload_started = Instant::now();
    let second = supervisor
        .activate(batch, serde_json::json!({}))
        .map_err(|error| error.to_string())?;
    let reload_elapsed = reload_started.elapsed();
    assert_ne!(second.id(), first.id());
    assert_eq!(
        supervisor.live_processes(),
        8,
        "the retired generation was not reaped from the budget"
    );
    let over = supervisor.prepare(over, serde_json::json!({}));
    assert!(
        matches!(&over, Err(PluginError::Registration(message)) if message.contains("exceed the reload-safe bound of 8")),
        "one plugin over the reload-safe bound at the default budget was not refused at admission: {over:?}"
    );
    assert!(
        supervisor
            .current_is_running(second.id())
            .map_err(|error| error.to_string())?,
        "the admission refusal disturbed the published generation"
    );
    supervisor.shutdown().map_err(|error| error.to_string())?;
    Ok(ExchangeReport {
        first: first_elapsed,
        reload: reload_elapsed,
        rendezvous: overlapped,
    })
}

/// The two-supervisor concurrent full-reload witness (PLR-R11, the
/// multi-instance resolution ruled on 2026-09-27): two supervisors -
/// two product instances - run in one process and share the one
/// systemd user manager. The containment and slice identity counters
/// are process-global, so each supervisor's units and slice are
/// distinct and each holds its own 16-process budget. Each activates
/// 8 plugins, the bounded rendezvous aligns the two supervisors, and
/// both full-budget reloads then run at the same time - 8 candidates
/// beside 8 published on each, 32 live plugin processes on the one
/// user manager at the peak - with the ninth declaration refused at
/// admission on each, naming the per-supervisor reload-safe bound.
/// The witness measures and prints the per-exchange elapsed of both
/// supervisors and asserts no aggregate bound: the per-supervisor
/// budget is not a deployment restriction, and the product claims no
/// N-instance activation capacity beyond this witnessed two-supervisor
/// full reload (CONTRACT.md, multi-instance envelope).
/// PLR-R12: a missed rendezvous (a partner that never became ready)
/// means the full-budget reloads did not overlap, so the witness fails
/// instead of passing with one side alone - the overlap is asserted,
/// not merely printed (review RETURN on dfcda61c, finding
/// PLR-R12-DFCDA61C-TWO-SUPERVISOR-OVERLAP-UNASSERTED).
#[test]
fn two_supervisors_share_the_user_manager_at_their_own_full_budgets() -> Result<(), Box<dyn Error>>
{
    let root =
        std::env::temp_dir().join(format!("secode-plugin-envelope-two-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    let mut batch = Vec::new();
    for index in 0..8 {
        let path = root.join(format!("plugin-{index}")).join("plugin.mjs");
        std::fs::create_dir_all(path.parent().ok_or("plugin parent")?)?;
        std::fs::write(
            &path,
            format!("export default {{id:'fixture.envelope.two.{index}',setup(){{}}}}\n"),
        )?;
        batch.extend(declarations(&path)?);
    }
    assert_eq!(batch.len(), 8);
    let over_path = root.join("plugin-8").join("plugin.mjs");
    std::fs::create_dir_all(over_path.parent().ok_or("plugin parent")?)?;
    std::fs::write(
        &over_path,
        "export default {id:'fixture.envelope.two.over',setup(){}}\n",
    )?;
    let mut over = batch.clone();
    over.extend(declarations(&over_path)?);
    assert_eq!(over.len(), 9);

    let ready_first = Arc::new(AtomicBool::new(false));
    let ready_second = Arc::new(AtomicBool::new(false));
    let (ready_first_b, ready_second_b) = (Arc::clone(&ready_first), Arc::clone(&ready_second));
    let (batch_first, over_first) = (batch.clone(), over.clone());
    let first_thread = std::thread::spawn(move || {
        one_supervisor_exchange(batch_first, over_first, ready_first_b, ready_second_b)
    });
    let second_thread =
        std::thread::spawn(move || one_supervisor_exchange(batch, over, ready_second, ready_first));
    let first_report = first_thread
        .join()
        .map_err(|_| "the first supervisor exchange panicked")??;
    let second_report = second_thread
        .join()
        .map_err(|_| "the second supervisor exchange panicked")??;
    let receipt = format!(
        "two-supervisor receipt: first {:?} / {:?}, second {:?} / {:?} (activation / full-budget reload, per exchange); reload overlap by rendezvous: {} / {}",
        first_report.first, first_report.reload, second_report.first, second_report.reload,
        first_report.rendezvous, second_report.rendezvous
    );
    // PLR-R12: the overlap IS the witness; a missed rendezvous is a
    // red, never a silent proceed-alone.
    assert!(
        first_report.rendezvous && second_report.rendezvous,
        "the two-supervisor full-budget reloads did not overlap: {receipt}"
    );
    eprintln!("{receipt}");
    std::fs::remove_dir_all(root)?;
    Ok(())
}

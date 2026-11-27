#[cfg(target_os = "linux")]
use super::resource::{linux_unit_executables, owned_linux_unit};
#[cfg(target_os = "linux")]
use super::unit_identity::systemd_units;
use super::*;
use tokio::time::Instant;

/// The cleanup grace this witness pays is a bridge-IPC round-trip (the
/// cleanup exchange), then the launcher-exit wait after the containment
/// stop. 24e timed the exchange out at the idle-box 100 ms on load 14-17
/// (`Timeout("cleanup")`). Sample the same round-trip on a live fixture
/// bridge first and size the grace from the worst sample; on an idle box
/// the samples stay single-digit milliseconds, the legacy 100 ms grace
/// holds, and the witness bound stays cleanup + reap + slack.
#[cfg(target_os = "linux")]
fn measured_cleanup_grace(plugin: &Path) -> Result<u64, Box<dyn Error>> {
    let (runtime, _root_cleanup) = runtime()?;
    let measurer = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let generation = gated_activate(&measurer, declarations(plugin)?, serde_json::json!({}))?;
    let registration = generation
        .registrations()
        .first()
        .ok_or("baseline bridge has no registration")?;
    let tool = registration
        .tools
        .first()
        .ok_or("baseline bridge has no echo tool")?;
    let worst = (0..5)
        .map(|_| {
            let began = Instant::now();
            measurer.invoke(
                generation.id(),
                &registration.plugin_id,
                tool.callback,
                serde_json::json!({"text": "baseline"}),
                serde_json::json!({"sessionID": "ses_baseline"}),
                CallAuthority::Unscoped,
            )?;
            Ok(began.elapsed().as_millis() as u64)
        })
        .collect::<Result<Vec<u64>, PluginError>>()?;
    measurer.shutdown()?;
    Ok(std::cmp::max(
        100,
        worst.into_iter().max().unwrap_or(100).saturating_mul(4),
    ))
}

/// The J01 regression witness: bounded shutdown of a SIGTERM-ignoring
/// bridge plus generation recovery. `shutdown()` returns Ok inside cleanup
/// plus reap, the exact unit is absent within the bound, and a second
/// generation recovers. Under load this is the path the base code timed
/// out on with a single immediate poll (J01, `receipts/repro-base.log`
/// and `receipts/ccm-battery-bridge-trimmed.log`). Honest scope (review
/// P3): the node fixture ignores SIGTERM, but the launcher (the
/// systemd-run wrapper) usually exits inside the cleanup grace, so the
/// post-kill path is NOT forced by this test; W2 (the `post_kill_reap`
/// seam witness in `src/supervisor/process.rs`) is the deterministic
/// kill-path witness.
#[cfg(target_os = "linux")]
#[test]
fn a_sigterm_ignoring_bridge_shuts_down_within_the_bound_and_recovers() -> Result<(), Box<dyn Error>>
{
    // The witness's cleanup grace is sized from a live-bridge round-trip
    // sample taken just before it (see measured_cleanup_grace).
    let (baseline_root, baseline_plugin) = fixture(
        "sigterm-ignored-baseline",
        "export default {id:'fixture.sigterm-ignored-baseline',async setup(ctx){ process.on('SIGTERM',()=>{}); await ctx.tool.transform((draft)=>draft.add({name:'fixture_baseline',description:'bounded baseline echo',args:{type:'object',additionalProperties:false,properties:{text:{type:'string'}},required:['text']},async execute(input){return {title:'fixture_baseline',output:input.text,metadata:{}}}})) }}\n",
    )?;
    let cleanup_millis = measured_cleanup_grace(&baseline_plugin)?;
    let (root, plugin) = fixture(
        "sigterm-ignored",
        "export default {id:'fixture.sigterm-ignored',setup(){ process.on('SIGTERM',()=>{}) }}\n",
    )?;
    let limits = BridgeLimits {
        cleanup_millis,
        reap_millis: 1_000,
        ..DEFAULT_BRIDGE_LIMITS
    };
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, limits)?;
    let executable = supervisor.runtime_identity().executable.clone();
    let pattern = format!("secode-plugin-{}-1-*.service", std::process::id());
    let before = systemd_units(&pattern)?;
    let first = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}))?;
    assert_eq!(first.id().value(), 1);
    let after = systemd_units(&pattern)?;
    let candidates: std::collections::BTreeSet<String> =
        after.difference(&before).cloned().collect();
    let unit = owned_linux_unit(&candidates, &executable, linux_unit_executables)?;
    let started = Instant::now();
    supervisor.shutdown()?;
    let elapsed = started.elapsed();
    let bound = Duration::from_millis(cleanup_millis + 1_000 + 1_000);
    assert!(
        elapsed <= bound,
        "termination of the containment child outlived cleanup + reap + slack: {elapsed:?} > {bound:?}"
    );
    assert_unit_absent_within(&unit, UNIT_ABSENCE_WAIT)?;
    let pattern2 = format!("secode-plugin-{}-2-*.service", std::process::id());
    let before2 = systemd_units(&pattern2)?;
    let second = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}))?;
    assert!(second.id().value() > first.id().value());
    assert!(supervisor.current_is_running(second.id())?);
    let after2 = systemd_units(&pattern2)?;
    let candidates2: std::collections::BTreeSet<String> =
        after2.difference(&before2).cloned().collect();
    let unit2 = owned_linux_unit(&candidates2, &executable, linux_unit_executables)?;
    supervisor.shutdown()?;
    assert_unit_absent_within(&unit2, UNIT_ABSENCE_WAIT)?;
    std::fs::remove_dir_all(root)?;
    std::fs::remove_dir_all(baseline_root)?;
    Ok(())
}

/// The test-side absence wait, kept separate from the product bound it
/// follows. The product's own bound (cleanup + reap + slack, asserted
/// earlier in the witness) is enforced independently of this poll
/// loop: this is only how long the witness keeps polling for the
/// unit's D-Bus-visible absence under carrier load. At sxs5 run
/// 20260927-020501 iteration c15-it2 (loadavg 39.97), the product
/// bound assertion passed in the same window and only this 5 s poll
/// loop tripped on the user manager's unit-unload lag, so the wait was
/// widened 5 s -> 15 s on that attribution (consolidated review
/// RETURN, finding B, 2026-09-27): no product bound moves, and zero
/// occurrences of this failure class are required in the next
/// comparison.
#[cfg(target_os = "linux")]
const UNIT_ABSENCE_WAIT: Duration = Duration::from_secs(15);

#[cfg(target_os = "linux")]
fn assert_unit_absent_within(unit: &str, bound: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + bound;
    loop {
        if systemd_units(unit)?.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("containment unit {unit} remained after its bound").into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

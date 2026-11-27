//! Resource-limit attribution, exact containment ownership and teardown.
#[cfg(target_os = "linux")]
use super::unit_identity::{self, systemd_units};
use super::*;

fn resource_violation(
    label: &str,
    execute: &str,
    limits: BridgeLimits,
    expected: &[&str],
) -> Result<(), Box<dyn Error>> {
    let source = format!(
        r#"
export default {{
  id: "fixture.{label}",
  async setup(ctx) {{
    await ctx.tool.transform((draft) => draft.add({{
      name: "fixture_{label}",
      args: {{type:"object"}},
      async execute() {{ {execute} }}
    }}))
  }}
}}
"#
    );
    let (root, plugin) = fixture(label, &source)?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, limits)?;
    let generation = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}))
        .map_err(|error| format!("{label} activation: {error}"))?;
    #[cfg(target_os = "linux")]
    let owned_unit = if label == "rss" {
        let pattern = format!("secode-plugin-{}-1-*.service", std::process::id());
        Some(owned_linux_unit(
            &systemd_units(&pattern)?,
            &supervisor.runtime_identity().executable,
            linux_unit_executables,
        )?)
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let teardown = OwnedUnitTeardown(owned_unit.clone());
    let registration = generation.registrations().first().ok_or("registration")?;
    let tool = registration.tools.first().ok_or("tool")?;
    let error = match supervisor.invoke(
        generation.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    ) {
        Ok(_) => panic!("resource-hostile plugin unexpectedly completed"),
        Err(error) => error.to_string(),
    };
    // The production stop is observed first, before the reason: without the
    // unit stop the call can end on its own deadline, and the absence is the
    // defect to name. The fixture teardown then reaps the owned unit even
    // when that judgment fails, and must be proven before the judgment may
    // score.
    #[cfg(target_os = "linux")]
    if let Some(unit) = owned_unit.as_ref() {
        let production =
            systemd_units(unit).and_then(|units| assert_owned_unit_absent(unit, &units));
        judged_after_teardown(production, teardown.finish())
            .map_err(|failure| format!("{label} invocation: {error}; containment: {failure}"))?;
    }
    assert!(
        expected.iter().any(|expected| error.contains(expected)),
        "{label} failed for wrong reason: {error}"
    );
    // The violation already invalidated and reaped the generation; retiring it
    // must neither resurrect cleanup nor leave an owned process behind.
    supervisor
        .shutdown()
        .map_err(|error| format!("{label} shutdown after attributed violation: {error}"))?;
    assert_eq!(supervisor.live_processes(), 0);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn owned_linux_unit(
    units: &std::collections::BTreeSet<String>,
    executable: &Path,
    mut inspect: impl FnMut(&str) -> Result<Vec<PathBuf>, Box<dyn Error>>,
) -> Result<String, Box<dyn Error>> {
    if units.len() > 4096 {
        return Err("fixture containment inventory exceeds its bound".into());
    }
    let mut owned = None;
    for unit in units {
        if inspect(unit)?.iter().any(|path| path == executable)
            && owned.replace(unit.clone()).is_some()
        {
            return Err("fixture containment ownership is ambiguous".into());
        }
    }
    owned.ok_or_else(|| "fixture containment ownership is missing".into())
}

#[cfg(target_os = "linux")]
#[path = "owned_teardown.rs"]
mod owned_teardown;
#[cfg(target_os = "linux")]
use owned_teardown::{judged_after_teardown, OwnedUnitTeardown};

/// Inspect actual unit members, allowing a sandbox launcher above Node.
/// An unrelated fixture that ends during the inventory contributes no paths.
#[cfg(target_os = "linux")]
pub(super) fn linux_unit_executables(unit: &str) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    unit_identity::unit_executables(unit)
}

#[cfg(target_os = "linux")]
fn assert_owned_unit_absent(
    owned: &str,
    units_after: &std::collections::BTreeSet<String>,
) -> Result<(), Box<dyn Error>> {
    if units_after.contains(owned) {
        return Err("OOM containment unit remained after the attributed failure".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn oom_cleanup_tolerates_siblings_but_refuses_owned_leak() -> Result<(), Box<dyn Error>> {
    let mut remaining = std::collections::BTreeSet::from(["sibling.service".to_owned()]);
    assert_owned_unit_absent("owned.service", &remaining)?;
    remaining.insert("owned.service".to_owned());
    assert!(
        assert_owned_unit_absent("owned.service", &remaining).is_err(),
        "owned unit leak escaped cleanup witness"
    );
    Ok(())
}

/// The ownership controls on a real generation with the real helper: the
/// verified runtime selects exactly one owned unit, a wrong runtime is
/// refused as missing ownership rather than attributed, and while the
/// production stop has not yet run the absence assertion refuses the live
/// owned unit. The production shutdown then leaves it absent with no live
/// process.
#[cfg(target_os = "linux")]
#[test]
fn the_executable_helper_binds_only_the_live_owned_unit() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "identity",
        "export default {id:'fixture.identity',setup(){}}\n",
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let pattern = format!("secode-plugin-{}-1-*.service", std::process::id());
    let before = systemd_units(&pattern)?;
    let activated = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}));
    let controls = activated.map_err(Box::<dyn Error>::from).and_then(|_| {
        let candidates: std::collections::BTreeSet<String> = systemd_units(&pattern)?
            .difference(&before)
            .cloned()
            .collect();
        let executable = supervisor.runtime_identity().executable.clone();
        let owned = owned_linux_unit(&candidates, &executable, linux_unit_executables)?;
        let wrong = owned_linux_unit(
            &candidates,
            Path::new("/nonexistent/wrong-runtime"),
            linux_unit_executables,
        );
        let live = assert_owned_unit_absent(&owned, &systemd_units(&owned)?);
        Ok((owned, wrong, live))
    });
    let shutdown = supervisor.shutdown();
    let (owned, wrong, live) = controls?;
    shutdown?;
    match wrong {
        Err(error) if error.to_string().contains("ownership is missing") => {}
        other => panic!("a wrong runtime was not refused as missing ownership: {other:?}"),
    }
    assert!(
        live.is_err(),
        "the absence assertion accepted a live owned unit before the production stop"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !systemd_units(&owned)?.is_empty() {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("owned unit {owned} remained after shutdown").into());
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(supervisor.live_processes(), 0);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn oom_ownership_requires_one_exact_runtime() -> Result<(), Box<dyn Error>> {
    let units = std::collections::BTreeSet::from([
        "owned.service".to_owned(),
        "sibling.service".to_owned(),
    ]);
    let executable = Path::new("/fixture/owned/node");
    let owned = owned_linux_unit(&units, executable, |unit| {
        Ok(vec![if unit == "owned.service" {
            executable.into()
        } else {
            "/fixture/sibling/node".into()
        }])
    })?;
    assert_eq!(owned, "owned.service");
    assert!(owned_linux_unit(&units, executable, |_| Ok(Vec::new())).is_err());
    assert!(owned_linux_unit(&units, executable, |_| Ok(vec![executable.into()])).is_err());
    assert!(owned_linux_unit(&units, executable, |_| Err("unreadable identity".into())).is_err());
    Ok(())
}

/// The observed RSS threshold stays below the hard cgroup ceiling by
/// MEMORY_MAX_HEADROOM_BYTES, so the monitor can attribute the violation
/// before the kernel's independent backstop fires.
const RSS_TEST_THRESHOLD_BYTES: u64 = 256 * 1024 * 1024;

#[test]
fn observed_rss_and_sustained_cpu_thresholds_terminate_the_owned_process(
) -> Result<(), Box<dyn Error>> {
    resource_violation(
        "rss",
        if cfg!(windows) {
            // Unbounded allocation exercises the native Job memory boundary
            // and the independent observed-RSS limit.
            "const held=[]; for (;;) held.push(Buffer.alloc(16*1024*1024, 1))"
        } else {
            // Paced hostile allocation: 16 MB per 100 ms (~160 MB/s), so the
            // charge growth between 50 ms watchdog samples stays far below
            // the threshold-to-ceiling headroom. The observed threshold is
            // then the first attributable trigger and the hard cgroup
            // ceiling remains the distinct fail-closed backstop.
            // Unbounded-rate allocation would make
            // threshold-versus-ceiling attribution a kernel preemption race.
            "const held=[]; for (;;) { held.push(Buffer.alloc(16*1024*1024, 1)); await new Promise((r) => setTimeout(r, 100)) }"
        },
        BridgeLimits {
            rss_bytes: RSS_TEST_THRESHOLD_BYTES,
            cpu_percent: u16::MAX,
            sample_millis: 50,
            call_millis: 10_000,
            cleanup_millis: 50,
            ..DEFAULT_BRIDGE_LIMITS
        },
        if cfg!(windows) {
            &["Failed to allocate memory", "RSS"]
        } else if cfg!(target_os = "macos") {
            // macOS names its watchdog attribution instead of a kernel bound.
            &[
                "exceeded the observed",
                "termination threshold",
                "watchdog-enforced, not kernel-enforced",
            ]
        } else {
            &["exceeded the observed", "termination threshold"]
        },
    )?;
    resource_violation(
        "cpu",
        "for (;;) {}",
        BridgeLimits {
            rss_bytes: 1024 * 1024 * 1024,
            cpu_percent: 50,
            cpu_violation_samples: 5,
            sample_millis: 100,
            call_millis: 10_000,
            cleanup_millis: 50,
            ..DEFAULT_BRIDGE_LIMITS
        },
        &["CPU"],
    )?;
    Ok(())
}

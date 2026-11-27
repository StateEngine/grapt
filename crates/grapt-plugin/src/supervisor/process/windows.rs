//! Native process-security policy, nested resource jobs and owned pipe drains.
use super::*;

pub(in crate::supervisor) fn spawn_bridge(
    _runtime: &VerifiedRuntime,
    containment: &AggregateContainment,
    limits: BridgeLimits,
    generation: PluginGenerationId,
    instance: u64,
    access: &super::super::isolation::ProcessAccess,
) -> Result<Spawned, PluginError> {
    if limits.tasks == 0 || limits.cpu_percent == 0 {
        return Err(PluginError::Runtime(
            "plugin Windows containment bounds must be nonzero".to_owned(),
        ));
    }
    // A nested Job's CPU rate is a share of its parent's rate, not of the
    // machine, so the per-process quota is expressed relative to the aggregate.
    let processors = std::thread::available_parallelism()
        .map(|count| count.get() as f64)
        .map_err(|error| PluginError::Spawn(error.to_string()))?;
    let requested = f64::from(limits.cpu_percent) / 100.0;
    let relative = (requested / containment.cpu_cores * processors).min(processors);
    let group = Arc::new(
        processkit::ProcessGroup::with_options(
            processkit::ProcessGroupOptions::default()
                .shutdown_timeout(Duration::from_millis(limits.cleanup_millis))
                .max_memory(limits.rss_bytes)
                .max_processes(u32::from(limits.tasks))
                .cpu_quota(relative),
        )
        .map_err(|error| PluginError::Spawn(error.to_string()))?,
    );
    let mut child = grapt_sandbox::spawn_piped(&access.request)
        .map_err(|error| PluginError::Spawn(format!("plugin_sandbox_unavailable: {error}; use a Windows build with native MXC process containment")))?;
    // The OS sandbox is active at creation. The trusted bridge does not load
    // guest code until the host sends its pinned source after both jobs adopt
    // it. MXC's kill-on-close job owns the child throughout this interval.
    let process_id = child.id();
    containment
        .group
        .adopt_external(process_id)
        .map_err(|error| PluginError::Spawn(format!("aggregate containment refused: {error}")))?;
    group
        .adopt_external(process_id)
        .map_err(|error| PluginError::Spawn(format!("process containment refused: {error}")))?;
    let pipes = child.take_stdin().zip(child.take_stdout());
    let child_stderr = child.take_stderr();
    let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let violation = Arc::new(Mutex::new(None));
    let (finished_sender, finished_receiver) = mpsc::sync_channel(1);
    let thread_exited = Arc::clone(&exited);
    let thread_violation = Arc::clone(&violation);
    let thread_group = Arc::clone(&group);
    let memory_limit = limits.rss_bytes;
    thread::Builder::new()
        .name(format!(
            "plugin-bridge-exit-{}-{instance}",
            generation.value()
        ))
        .spawn(move || {
            let status = child.wait();
            let attribution = status.as_ref().map(|code| *code == 0).unwrap_or(false);
            attribute_job_memory(&thread_group, memory_limit, attribution, &thread_violation);
            thread_exited.store(true, Ordering::Release);
            let _ = finished_sender.send(status.map(|_| ()).map_err(|error| error.to_string()));
        })
        .map_err(|error| PluginError::Spawn(error.to_string()))?;
    let control = ProcessControl {
        violation,
        containment: ProcessContainment::Windows { group },
        completion: Arc::new(Mutex::new(Some(finished_receiver))),
        exited,
        cleanup: Duration::from_millis(limits.cleanup_millis),
    };
    if let Some(mut stderr) = child_stderr {
        let diagnostics = control.clone();
        if let Err(error) = thread::Builder::new().name("plugin-sandbox-stderr".into())
            .spawn(move || {
                let outcome = std::io::copy(&mut stderr.by_ref().take(1024 * 1024 + 1), &mut std::io::sink());
                if !matches!(outcome, Ok(bytes) if bytes <= 1024 * 1024) {
                    let _ = diagnostics.terminate_for(ViolationKind::Resource,
                        "plugin_diagnostics_exhausted: diagnostics exceeded 1 MiB or became unreadable; reduce plugin console output".into());
                }
            }) {
                let _ = control.terminate();
                return Err(PluginError::Spawn(error.to_string()));
            }
    }
    let Some((input, output)) = pipes else {
        let _ = control.terminate();
        return Err(PluginError::Spawn(
            "bridge pipes were not established".to_owned(),
        ));
    };
    let quota = requested.min(containment.cpu_cores);
    if let Err(error) = start_resource_watchdog(control.clone(), process_id, limits, quota) {
        let _ = control.terminate();
        return Err(error);
    }
    Ok(Spawned {
        input: BridgeInput::Windows(input),
        output,
        control,
    })
}

/// A Job Object records no post-mortem limit verdict, so a non-zero exit
/// whose committed-memory peak reached the hard limit is attributed to it.
fn attribute_job_memory(
    group: &processkit::ProcessGroup,
    limit: u64,
    success: bool,
    violation: &Mutex<Option<Violation>>,
) {
    const MARGIN: u64 = 4 * 1024 * 1024;
    let failed = !success;
    let peak = group.stats().ok().and_then(|stats| stats.peak_memory_bytes);
    if let (true, Some(peak)) = (failed, peak) {
        if peak.saturating_add(MARGIN) >= limit {
            if let Ok(mut held) = violation.lock() {
                held.get_or_insert_with(|| {
                    Violation {
                        kind: ViolationKind::Resource,
                        reason: format!(
                            "plugin bridge committed memory peaked at {peak} bytes against its {limit}-byte hard Job memory boundary"
                        ),
                    }
                });
            }
        }
    }
}

/// Windows attribution: resident memory from the process table, and CPU quota
/// exhaustion from Job accounting. A hard Job CPU cap duty-cycles the process,
/// so point samples alternate between zero and bursts; accumulated CPU time
/// over the sampling window is the reliable measure.
fn start_resource_watchdog(
    control: ProcessControl,
    process_id: u32,
    limits: BridgeLimits,
    quota: f64,
) -> Result<(), PluginError> {
    let ProcessContainment::Windows { group } = &control.containment;
    let group = Arc::clone(group);
    thread::Builder::new()
        .name(format!("plugin-resource-{process_id}"))
        .spawn(move || {
            let pid = Pid::from_u32(process_id);
            let mut system = System::new_all();
            let mut missing = 0u8;
            let sample = Duration::from_millis(limits.sample_millis.max(10));
            let window = usize::from(limits.cpu_violation_samples.max(1));
            let mut accounting = std::collections::VecDeque::with_capacity(window + 1);
            loop {
                thread::sleep(sample);
                if control.has_exited().unwrap_or(true) {
                    return;
                }
                system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
                let Some(process) = system.process(pid) else {
                    missing = missing.saturating_add(1);
                    if missing >= 3 {
                        let _ = control.terminate_for(
                            ViolationKind::Other,
                            "plugin resource telemetry became unavailable; the owned bridge was terminated fail-closed"
                                .to_owned(),
                        );
                        return;
                    }
                    continue;
                };
                missing = 0;
                if process.memory() > limits.rss_bytes {
                    let _ = control.terminate_for(ViolationKind::Resource, format!(
                        "plugin bridge RSS {} exceeded the observed {}-byte termination threshold",
                        process.memory(),
                        limits.rss_bytes
                    ));
                    return;
                }
                let Some(cpu) = group.stats().ok().and_then(|stats| stats.total_cpu_time) else {
                    let _ = control.terminate_for(
                        ViolationKind::Other,
                        "plugin Job CPU accounting became unavailable; the owned bridge was terminated fail-closed"
                            .to_owned(),
                    );
                    return;
                };
                accounting.push_back((tokio::time::Instant::now(), cpu));
                if accounting.len() > window + 1 {
                    accounting.pop_front();
                }
                if accounting.len() <= window {
                    continue;
                }
                let (Some((first_at, first_cpu)), Some((last_at, last_cpu))) =
                    (accounting.front(), accounting.back())
                else {
                    continue;
                };
                let wall = last_at.duration_since(*first_at).as_secs_f64();
                let used = last_cpu.saturating_sub(*first_cpu).as_secs_f64();
                if wall > 0.0 && used / wall >= quota * 0.85 {
                    let _ = control.terminate_for(ViolationKind::Resource, format!(
                        "plugin bridge CPU exhausted its {}% Job CPU quota across {} samples; the owned process tree was terminated",
                        limits.cpu_percent, limits.cpu_violation_samples
                    ));
                    return;
                }
            }
        })
        .map(|_| ())
        .map_err(|error| PluginError::Spawn(error.to_string()))
}

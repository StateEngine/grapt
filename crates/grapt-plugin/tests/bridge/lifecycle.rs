#[cfg(target_os = "linux")]
use super::unit_identity::systemd_units;
use super::*;

#[cfg(target_os = "linux")]
#[test]
fn bridge_process_uses_the_declared_hard_cgroup_limits() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "linux-containment",
        "export default {id:'fixture.contained',setup(){}}\n",
    )?;
    let limits = BridgeLimits {
        rss_bytes: DEFAULT_BRIDGE_LIMITS.rss_bytes + 1024 * 1024,
        cpu_percent: DEFAULT_BRIDGE_LIMITS.cpu_percent - 1,
        tasks: DEFAULT_BRIDGE_LIMITS.tasks - 1,
        ..DEFAULT_BRIDGE_LIMITS
    };
    let unit_pattern = format!("secode-plugin-{}-1-*.service", std::process::id());
    let before = systemd_units(&unit_pattern)?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, limits)?;
    let generation = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}))?;
    assert_eq!(generation.id().value(), 1);
    let after = systemd_units(&unit_pattern)?;
    let candidates = after.difference(&before).collect::<Vec<_>>();
    let mut selected = None;
    for candidate in candidates {
        let output = Command::new("/usr/bin/systemctl")
            .args([
                "--user",
                "show",
                candidate,
                "--property=ControlGroup,MemoryMax,MemorySwapMax,CPUQuotaPerSecUSec,TasksMax,KillMode,SendSIGKILL,OOMPolicy,SuccessExitStatus,RemainAfterExit,ActiveState",
            ])
            .output()?;
        if !output.status.success() {
            continue;
        }
        let properties = String::from_utf8(output.stdout)?;
        if properties.contains("ActiveState=active")
            && properties.contains(&format!(
                "MemoryMax={}",
                limits.rss_bytes.saturating_add(MEMORY_MAX_HEADROOM_BYTES)
            ))
            && properties.contains(&format!("TasksMax={}", limits.tasks))
        {
            selected = Some((candidate, properties));
            break;
        }
    }
    let (unit, properties) =
        selected.ok_or("activation did not publish its unique containment unit")?;
    assert!(properties.contains("ActiveState=active"));
    assert!(properties.contains(&format!(
        "MemoryMax={}",
        limits.rss_bytes.saturating_add(MEMORY_MAX_HEADROOM_BYTES)
    )));
    assert!(properties.contains("MemorySwapMax=0"));
    assert!(properties.contains(&format!(
        "CPUQuotaPerSecUSec={}ms",
        limits.cpu_percent.saturating_mul(10)
    )));
    assert!(properties.contains(&format!("TasksMax={}", limits.tasks)));
    assert!(properties.contains("KillMode=control-group"));
    assert!(properties.contains("SendSIGKILL=yes"));
    assert!(
        properties.contains("OOMPolicy=continue"),
        "plugin OOM policy can emit a desktop low-memory notification: {properties}"
    );
    assert!(
        properties.contains("SuccessExitStatus=KILL"),
        "plugin OOM exit is not classified as successful: {properties}"
    );
    assert!(
        properties.contains("RemainAfterExit=yes"),
        "plugin OOM evidence is not retained for inspection: {properties}"
    );
    let cgroup = properties
        .lines()
        .find_map(|line| line.strip_prefix("ControlGroup="))
        .ok_or("bridge cgroup path is absent")?;
    let members = std::fs::read_to_string(format!("/sys/fs/cgroup{cgroup}/cgroup.procs"))?;
    assert!(!members.trim().is_empty());
    supervisor.shutdown()?;
    let inactive = Command::new("/usr/bin/systemctl")
        .args(["--user", "is-active", "--quiet", unit])
        .status()?;
    assert!(!inactive.success());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_job_contains_and_reaps_an_owned_descendant() -> Result<(), Box<dyn Error>> {
    use std::io::{BufRead, BufReader};

    let (runtime, _root_cleanup) = runtime()?;
    let executable = runtime.executable;
    let outer = windows_spawn::Job::create()?;
    outer.set_kill_on_close(true)?;
    let group = processkit::ProcessGroup::with_options(
        processkit::ProcessGroupOptions::default()
            .max_memory(512 * 1024 * 1024)
            .max_processes(4)
            .cpu_quota(0.5),
    )?;
    assert_eq!(group.mechanism(), processkit::Mechanism::JobObject);
    let mut command = windows_spawn::Command::new(executable);
    command
        .args([
            "-e",
            "const {spawn}=require('node:child_process');const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});console.log(child.pid);setInterval(()=>{},1000)",
        ])
        .stdin(windows_spawn::Stdio::null())
        .stdout(windows_spawn::Stdio::piped())
        .stderr(windows_spawn::Stdio::null());
    let suspended = command.spawn_suspended_with(
        windows_spawn::SpawnOptions::new()
            .job(&outer)
            .creation_flags(windows_spawn::CreationFlags::NO_WINDOW),
    )?;
    let root = suspended.id();
    assert!(group.members()?.is_empty());
    group.adopt_external(root)?;
    assert!(group.members()?.contains(&root));
    let mut child = suspended.resume()?;
    let output = child
        .stdout
        .take()
        .ok_or("descendant stdout was not piped")?;
    let mut reader = BufReader::new(output);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let descendant = line.trim().parse::<u32>()?;
    assert!(group.members()?.contains(&descendant));
    assert!(group.members()?.len() >= 2);
    drop(reader);
    drop(outer);
    let _finished = child.wait()?;
    assert!(group.members()?.is_empty());
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_assignment_failure_reaps_before_resume() -> Result<(), Box<dyn Error>> {
    let (root, _) = fixture("windows-assignment", "export default {}\n")?;
    let marker = root.join("resumed.txt");
    let (runtime, _root_cleanup) = runtime()?;
    let executable = runtime.executable;
    let group = processkit::ProcessGroup::with_options(
        processkit::ProcessGroupOptions::default()
            .max_memory(512 * 1024 * 1024)
            .max_processes(1)
            .cpu_quota(1.0),
    )?;
    let mut keeper_command = windows_spawn::Command::new(&executable);
    keeper_command
        .args(["-e", "setInterval(()=>{},1000)"])
        .stdin(windows_spawn::Stdio::null())
        .stdout(windows_spawn::Stdio::null())
        .stderr(windows_spawn::Stdio::null());
    let keeper_suspended = keeper_command.spawn_suspended_with(
        windows_spawn::SpawnOptions::new().creation_flags(windows_spawn::CreationFlags::NO_WINDOW),
    )?;
    let keeper_pid = keeper_suspended.id();
    group.adopt_external(keeper_pid)?;
    let mut keeper = keeper_suspended.resume()?;

    let outer = windows_spawn::Job::create()?;
    outer.set_kill_on_close(true)?;
    let mut attempted = windows_spawn::Command::new(&executable);
    attempted
        .args([
            "-e",
            &format!(
                "require('node:fs').writeFileSync({},'resumed')",
                serde_json::to_string(&marker.to_string_lossy())?
            ),
        ])
        .stdin(windows_spawn::Stdio::null())
        .stdout(windows_spawn::Stdio::null())
        .stderr(windows_spawn::Stdio::null());
    let refused = attempted.spawn_suspended_with(
        windows_spawn::SpawnOptions::new()
            .job(&outer)
            .creation_flags(windows_spawn::CreationFlags::NO_WINDOW),
    )?;
    let refused_pid = refused.id();
    if group.adopt_external(refused_pid).is_ok() {
        drop(refused);
        group.kill_all()?;
        let _finished = keeper.wait()?;
        return Err("a second process escaped the active-process limit".into());
    }
    assert!(!marker.exists());
    drop(refused);
    drop(outer);
    let live_nodes = group
        .members_info()?
        .into_iter()
        .filter(|member| {
            member
                .exe_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("node.exe"))
        })
        .map(|member| member.pid())
        .collect::<Vec<_>>();
    assert_eq!(live_nodes, [keeper_pid]);
    group.kill_all()?;
    let _finished = keeper.wait()?;
    assert!(group.members()?.is_empty());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_outer_job_close_reaps_pre_adoption_suspended_child() -> Result<(), Box<dyn Error>> {
    let (root, _) = fixture("windows-pre-adoption", "export default {}\n")?;
    let marker = root.join("resumed.txt");
    let (runtime, _root_cleanup) = runtime()?;
    let executable = runtime.executable;
    let outer = windows_spawn::Job::create()?;
    outer.set_kill_on_close(true)?;
    let mut command = windows_spawn::Command::new(executable);
    command
        .args([
            "-e",
            &format!(
                "require('node:fs').writeFileSync({},'resumed')",
                serde_json::to_string(&marker.to_string_lossy())?
            ),
        ])
        .stdin(windows_spawn::Stdio::null())
        .stdout(windows_spawn::Stdio::null())
        .stderr(windows_spawn::Stdio::null());
    let suspended = command.spawn_suspended_with(
        windows_spawn::SpawnOptions::new()
            .job(&outer)
            .creation_flags(windows_spawn::CreationFlags::NO_WINDOW),
    )?;
    assert!(!marker.exists());
    drop(outer);
    match suspended.resume() {
        Ok(mut child) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("a child resumed after its kill-on-close owner was dropped".into());
        }
        Err(_error) => {}
    }
    assert!(!marker.exists());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn changed_source_is_refused_before_activation() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture("toctou", "export default {id:'before',setup(){}}\n")?;
    let declarations = declarations(&plugin)?;
    std::fs::write(&plugin, "export default {id:'after',setup(){}}\n")?;
    let limits = BridgeLimits {
        startup_millis: 2_000,
        cleanup_millis: 100,
        ..DEFAULT_BRIDGE_LIMITS
    };
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, limits)?;
    let activation = gated_activate(&supervisor, declarations, serde_json::json!({}));
    assert!(
        matches!(activation, Err(PluginError::Runtime(message)) if message.contains("changed"))
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn cancellation_reaps_the_owned_generation_and_a_new_generation_recovers(
) -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "cancel",
        r#"
export default {
  id: "fixture.cancel",
  async setup(ctx) {
    await ctx.tool.transform((draft) => draft.add({
      name: "fixture_hang",
      args: {type:"object",additionalProperties:false},
      async execute() {
        await ctx.session.get({})
        return new Promise(() => {})
      }
    }))
  }
}
"#,
    )?;
    let limits = BridgeLimits {
        call_millis: 30_000,
        cleanup_millis: 100,
        ..DEFAULT_BRIDGE_LIMITS
    };
    let (runtime, _root_cleanup) = runtime()?;
    struct InvocationStarted(mpsc::SyncSender<()>);
    impl PluginHost for InvocationStarted {
        fn call(
            &self,
            _context: &PluginHostContext,
            operation: HostOperation,
            _payload: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            if operation != HostOperation::SessionGet {
                return Err("unexpected cancellation fixture host operation".to_owned());
            }
            self.0.try_send(()).map_err(|error| error.to_string())?;
            Ok(serde_json::Value::Null)
        }
    }
    let (started, admitted) = mpsc::sync_channel(1);
    let supervisor = Arc::new(PluginSupervisor::with_host(
        runtime,
        limits,
        Arc::new(InvocationStarted(started)),
    )?);
    let outcome = (|| -> Result<(), Box<dyn Error>> {
        let declaration = declarations(&plugin)?;
        let generation = gated_activate(&supervisor, declaration.clone(), serde_json::json!({}))?;
        let registration = generation.registrations().first().ok_or("registration")?;
        let tool = registration.tools.first().ok_or("tool")?;
        let route = (
            generation.id(),
            registration.plugin_id.clone(),
            tool.callback,
        );
        let invoked = Arc::clone(&supervisor);
        let (sent, received) = mpsc::sync_channel(1);
        let invocation = thread::spawn(move || {
            let result = invoked.invoke(
                route.0,
                &route.1,
                route.2,
                serde_json::json!({}),
                serde_json::json!({}),
                CallAuthority::Unscoped,
            );
            let _ = sent.send(result);
        });
        // A guest host callback proves invocation has crossed admission. A sleep
        // can cancel before the worker enters invoke and legitimately return stale.
        let started = admitted.recv_timeout(Duration::from_secs(2));
        let cancellation = supervisor.cancel_generation(generation.id());
        let cancelled = received
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "owned plugin generation remained live after cancellation");
        let join_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !invocation.is_finished() && tokio::time::Instant::now() < join_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let joined = if invocation.is_finished() {
            invocation.join().map_err(|_| "invocation worker panicked")
        } else {
            Err("invocation worker remained live after cancellation")
        };
        started
            .map_err(|_| "guest invocation did not signal admission within the fixture bound")?;
        if !cancellation? {
            return Err("owned generation was absent at cancellation".into());
        }
        joined?;
        let cancelled = cancelled?;
        if !matches!(&cancelled, Err(PluginError::Protocol(message)) if message.contains("exited"))
        {
            return Err(format!("cancellation returned the wrong outcome: {cancelled:?}").into());
        }
        if supervisor.current()?.is_some() {
            return Err("cancelled generation remained current".into());
        }

        let recovered = gated_activate(&supervisor, declaration, serde_json::json!({}))?;
        if recovered.id().value() <= generation.id().value() {
            return Err("recovery did not publish a newer generation".into());
        }
        Ok(())
    })();
    let shutdown = supervisor.shutdown();
    let removed = std::fs::remove_dir_all(root);
    shutdown?;
    removed?;
    outcome
}

#[test]
fn hung_cleanup_is_bounded_reported_and_followed_by_recovery() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "hung-cleanup",
        r#"
export default {
  id: "fixture.hung-cleanup",
  async setup() { return async () => new Promise(() => {}) }
}
"#,
    )?;
    let limits = BridgeLimits {
        cleanup_millis: 100,
        ..DEFAULT_BRIDGE_LIMITS
    };
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, limits)?;
    let declaration = declarations(&plugin)?;
    let first = gated_activate(&supervisor, declaration.clone(), serde_json::json!({}))?;
    let shutdown = supervisor.shutdown();
    assert!(matches!(shutdown, Err(PluginError::Timeout("cleanup"))));
    assert!(supervisor.current()?.is_none());
    let recovered = gated_activate(&supervisor, declaration, serde_json::json!({}))?;
    assert!(recovered.id().value() > first.id().value());
    let _ = supervisor.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn unexpected_process_loss_is_reported_and_a_successor_generation_recovers(
) -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "process-loss",
        r#"
export default {
  id: "fixture.process-loss",
  async setup(ctx) {
    await ctx.tool.transform((draft) => draft.add({
      name: "fixture_exit",
      args: {type:"object"},
      execute() { process.exit(71) }
    }))
  }
}
"#,
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let declaration = declarations(&plugin)?;
    let first = gated_activate(&supervisor, declaration.clone(), serde_json::json!({}))?;
    let registration = first.registrations().first().ok_or("registration")?;
    let tool = registration.tools.first().ok_or("tool")?;
    let lost = supervisor.invoke(
        first.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    );
    assert!(
        matches!(lost, Err(PluginError::Protocol(ref message)) if message.contains("exited")),
        "the lost process was not classified as exited: {lost:?}"
    );
    assert!(!supervisor.current_is_running(first.id())?);
    let recovered = gated_activate(&supervisor, declaration, serde_json::json!({}))?;
    assert!(recovered.id().value() > first.id().value());
    assert!(supervisor.current_is_running(recovered.id())?);
    assert!(
        !supervisor.current_is_running(first.id())?,
        "a retired generation id was reported as the running generation"
    );
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[ignore = "evidence witness requires the source-pinned oracle dependency tree"]
fn effect_form_registers_and_executes_through_the_same_bridge() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "effect",
        r#"
import * as Effect from "effect/Effect"
export default {
  id: "fixture.effect",
  effect: (ctx) => ctx.tool.transform((draft) => draft.add({
    name: "fixture_effect",
    args: {type:"object",additionalProperties:false,properties:{text:{type:"string"}},required:["text"]},
    execute: (input, context) => Effect.succeed({
      title: "effect",
      output: `${input.text}:${context.sessionID}`,
      metadata: {}
    })
  })).pipe(Effect.asVoid)
}
"#,
    )?;
    copy_tree(&oracle_effect_package()?, &root.join("node_modules/effect"))?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let generation = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}))?;
    let registration = generation.registrations().first().ok_or("registration")?;
    let tool = registration.tools.first().ok_or("tool")?;
    let output = supervisor.invoke(
        generation.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({"text":"effect-ok"}),
        serde_json::json!({"sessionID":"ses_effect"}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(output["output"], "effect-ok:ses_effect");
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn failed_partial_setup_rolls_back_and_preserves_the_prior_generation() -> Result<(), Box<dyn Error>>
{
    let (root, good) = fixture(
        "rollback",
        r#"
export default {
  id: "fixture.good",
  async setup(ctx) {
    await ctx.tool.transform((draft) => draft.add({
      name: "fixture_good",
      args: {type:"object"},
      async execute() { return {title:"good",output:"still-active",metadata:{}} }
    }))
  }
}
"#,
    )?;
    let bad = root.join("bad.mjs");
    std::fs::write(
        &bad,
        "export default {id:'fixture.bad',setup(){throw new Error('deliberate setup failure')}}\n",
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let first = gated_activate(&supervisor, declarations(&good)?, serde_json::json!({}))?;
    let failed = gated_activate(&supervisor, declarations(&bad)?, serde_json::json!({}));
    assert!(
        matches!(failed, Err(PluginError::Invocation(message)) if message.contains("deliberate setup failure"))
    );
    assert_eq!(
        supervisor.current()?.as_ref().map(|value| value.id()),
        Some(first.id())
    );
    let registration = first.registrations().first().ok_or("registration")?;
    let tool = registration.tools.first().ok_or("tool")?;
    let result = supervisor.invoke(
        first.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(result["output"], "still-active");
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// A plugin that cannot start, or times out starting, costs only itself: its
/// sibling still registers and publishes, and the candidate names the plugins
/// it runs without.
#[test]
fn one_failed_plugin_leaves_its_siblings_running() -> Result<(), Box<dyn Error>> {
    let (root, good) = fixture(
        "sibling",
        "export default {id:'fixture.sibling',async setup(){}}\n",
    )?;
    let bad = root.join("bad.mjs");
    std::fs::write(
        &bad,
        "export default {id:'fixture.bad',setup(){throw new Error('deliberate setup failure')}}\n",
    )?;
    let slow = root.join("slow.mjs");
    std::fs::write(
        &slow,
        "export default {id:'fixture.slow',setup(){return new Promise(()=>{})}}\n",
    )?;
    let mut both = declarations(&good)?;
    both.extend(declarations(&bad)?);
    both.extend(declarations(&slow)?);
    let (runtime, _root_cleanup) = runtime()?;
    let limits = BridgeLimits {
        startup_millis: 2_000,
        cleanup_millis: 100,
        ..DEFAULT_BRIDGE_LIMITS
    };
    let supervisor = PluginSupervisor::new(runtime, limits)?;
    let candidate = {
        let _permit = FixtureActivationPermit::acquire();
        supervisor.prepare(both, serde_json::json!({}))?
    };
    let skipped = candidate.skipped();
    assert_eq!(skipped.len(), 2, "{skipped:?}");
    assert!(skipped[1].target.contains("slow.mjs"), "{skipped:?}");
    assert!(skipped[0].target.contains("bad.mjs"), "{skipped:?}");
    assert!(
        skipped[0].reason.contains("deliberate setup failure"),
        "{skipped:?}"
    );
    let ids: Vec<_> = candidate
        .generation()
        .registrations()
        .iter()
        .map(|r| r.plugin_id.clone())
        .collect();
    assert_eq!(ids, ["fixture.sibling"]);
    supervisor.publish(candidate)?;
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn malformed_oversized_crashed_late_and_duplicate_bridge_frames_fail_closed(
) -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture("hostile-frames", "export default {id:'unused',setup(){}}\n")?;
    let declarations = declarations(&plugin)?;
    let cases = [
        (
            "crash",
            "process.stdin.once('data',()=>process.exit(23))\n",
            "exited",
            1024usize,
            DEFAULT_BRIDGE_LIMITS.startup_millis,
        ),
        (
            "malformed",
            "process.stdin.once('data',()=>process.stdout.write('{bad-json}\\n'))\n",
            "line 1",
            1024,
            DEFAULT_BRIDGE_LIMITS.startup_millis,
        ),
        (
            "oversized",
            "process.stdin.once('data',()=>process.stdout.write('x'.repeat(2048)+'\\n'))\n",
            "frame bound",
            1024,
            DEFAULT_BRIDGE_LIMITS.startup_millis,
        ),
        (
            "late",
            "process.stdin.once('data',()=>setTimeout(()=>process.stdout.write('{}\\n'),250))\n",
            "timed out",
            1024,
            50,
        ),
    ];
    for (label, source, expected, frame_bytes, startup_millis) in cases {
        let bridge = custom_bridge(&root, label, source)?;
        let (runtime, _root_cleanup) = runtime_for(&bridge)?;
        let supervisor = PluginSupervisor::new(
            runtime,
            BridgeLimits {
                frame_bytes,
                startup_millis,
                cleanup_millis: 50,
                ..DEFAULT_BRIDGE_LIMITS
            },
        )?;
        let error = match gated_activate(&supervisor, declarations.clone(), serde_json::json!({})) {
            Ok(_) => panic!("hostile bridge activation unexpectedly succeeded"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains(expected),
            "{label} failed for wrong reason: {error}"
        );
    }

    let duplicate = custom_bridge(
        &root,
        "duplicate",
        r#"
import readline from "node:readline"
const lines = readline.createInterface({input:process.stdin,crlfDelay:Infinity})
const payloads = {
  "pin-files": {pinned: 1},
  "load": {plugin_id: "fixture.duplicate"},
  "activate": {plugin_id:"fixture.duplicate",tools:[{name:"duplicate_tool",description:null,input_schema:{type:"object"},callback:1}],callbacks:[],capabilities:[]},
  "invoke-tool": {output: "first"},
  "cleanup": {cleaned: true},
}
for await (const line of lines) {
  const request = JSON.parse(line)
  const response = {
    version:2,generation:request.generation,correlation:request.correlation,outcome:"ok",
    payload:payloads[request.operation] ?? null
  }
  process.stdout.write(JSON.stringify(response)+"\n")
  if (request.operation === "invoke-tool") process.stdout.write(JSON.stringify(response)+"\n")
}
"#,
    )?;
    let (runtime, _root_cleanup) = runtime_for(&duplicate)?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let generation = gated_activate(&supervisor, declarations, serde_json::json!({}))?;
    let registration = generation.registrations().first().ok_or("registration")?;
    let tool = registration.tools.first().ok_or("tool")?;
    let first = supervisor.invoke(
        generation.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(first["output"], "first");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while supervisor.current_is_running(generation.id())? {
        if tokio::time::Instant::now() > deadline {
            return Err("a duplicated reply did not invalidate its generation".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    let duplicate_error = match supervisor.invoke(
        generation.id(),
        &registration.plugin_id,
        tool.callback,
        serde_json::json!({}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    ) {
        Ok(_) => panic!("an invalidated generation accepted another call"),
        Err(error) => error.to_string(),
    };
    assert!(
        duplicate_error.contains("unknown, late, or duplicated"),
        "duplicate reply was refused for the wrong reason: {duplicate_error}"
    );
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

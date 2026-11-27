//! Transport corruption, deadlines, call grants, cancellation, candidates, and
//! the supervisor-wide process budget.

use super::*;
use grapt_plugin::{SupervisorBudget, DEFAULT_SUPERVISOR_BUDGET};

fn first_tool(
    generation: &grapt_plugin::PluginGeneration,
    plugin: &str,
    name: &str,
) -> Result<u64, Box<dyn Error>> {
    Ok(generation
        .registrations()
        .iter()
        .find(|registration| registration.plugin_id == plugin)
        .ok_or("registration")?
        .tools
        .iter()
        .find(|tool| tool.name == name)
        .ok_or("tool")?
        .callback)
}

fn scoped(call: &str) -> CallAuthority {
    CallAuthority::Scoped {
        scope: Arc::new(call.to_owned()),
        call: call.to_owned(),
    }
}

#[test]
fn a_call_deadline_invalidates_the_generation_and_a_successor_recovers(
) -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "deadline",
        r#"
export default {
  id: "fixture.deadline",
  async setup(ctx) {
    await ctx.tool.transform((draft) => draft.add({
      name: "hang", args: {type:"object"}, async execute() { return new Promise(() => {}) }
    }))
  }
}
"#,
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(
        runtime,
        BridgeLimits {
            call_millis: 500,
            cleanup_millis: 200,
            ..DEFAULT_BRIDGE_LIMITS
        },
    )?;
    let declaration = declarations(&plugin)?;
    let first = gated_activate(&supervisor, declaration.clone(), serde_json::json!({}))?;
    let callback = first_tool(&first, "fixture.deadline", "hang")?;
    let timed_out = supervisor.invoke(
        first.id(),
        "fixture.deadline",
        callback,
        serde_json::json!({}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    );
    assert!(
        matches!(timed_out, Err(PluginError::Timeout("tool invocation"))),
        "a hung call did not reach its deadline: {timed_out:?}"
    );
    assert!(
        !supervisor.current_is_running(first.id())?,
        "a timed-out generation kept serving with untrustworthy correlation state"
    );
    let reused = supervisor.invoke(
        first.id(),
        "fixture.deadline",
        callback,
        serde_json::json!({}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    );
    assert!(
        matches!(&reused, Err(PluginError::Protocol(message)) if message.contains("timed out")),
        "an invalidated generation accepted a later call: {reused:?}"
    );
    let recovered = gated_activate(&supervisor, declaration, serde_json::json!({}))?;
    assert!(supervisor.current_is_running(recovered.id())?);
    supervisor.shutdown()?;
    assert_eq!(supervisor.live_processes(), 0);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn an_unterminated_oversized_frame_is_refused_before_it_can_grow() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture("unterminated", "export default {id:'unused',setup(){}}\n")?;
    let bridge = custom_bridge(
        &root,
        "unterminated",
        "process.stdin.once('data',()=>{const chunk='x'.repeat(65536);setInterval(()=>process.stdout.write(chunk),1)})\n",
    )?;
    let (runtime, _root_cleanup) = runtime_for(&bridge)?;
    let supervisor = PluginSupervisor::new(
        runtime,
        BridgeLimits {
            frame_bytes: 1024 * 1024,
            startup_millis: 3_000,
            cleanup_millis: 200,
            ..DEFAULT_BRIDGE_LIMITS
        },
    )?;
    let refused = gated_activate(&supervisor, declarations(&plugin)?, serde_json::json!({}));
    assert!(
        matches!(&refused, Err(error) if error.to_string().contains("before its terminator")),
        "an unterminated frame was not refused at the frame bound: {refused:?}"
    );
    assert_eq!(supervisor.live_processes(), 0);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[derive(Debug, Default)]
struct GrantHost {
    granted: Mutex<Vec<bool>>,
}

impl PluginHost for GrantHost {
    fn call(
        &self,
        context: &PluginHostContext,
        _operation: HostOperation,
        _payload: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.granted
            .lock()
            .map_err(|_| "grant host poisoned".to_owned())?
            .push(context.grant().is_some());
        context
            .grant()
            .map(|grant| serde_json::json!({"grant": grant.id()}))
            .ok_or_else(|| "fixture host requires a live grant".to_owned())
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn concurrent_calls_are_serialized_and_a_finished_call_grant_cannot_be_replayed(
) -> Result<(), Box<dyn Error>> {
    let (root, _plugin) = fixture(
        "grants",
        r#"
import { rename, writeFile } from "node:fs/promises"
const spans = []
export default {
  id: "fixture.grants",
  async setup(ctx) {
    await ctx.tool.transform((draft) => draft.add({
      name: "slow",
      args: {type:"object"},
      async execute(input) {
        const started = Date.now()
        const granted = await ctx.session.get({})
        await new Promise((resolve) => setTimeout(resolve, 250))
        spans.push([started, Date.now()])
        if (input.replay) {
          setTimeout(async () => {
            let outcome
            try { await ctx.session.get({}); outcome = "replayed" } catch (error) { outcome = String(error.message) }
            // Written aside and renamed into place, so the witness can never
            // read the file between its creation and its content.
            await writeFile(input.replay + ".partial", outcome, "utf8")
            await rename(input.replay + ".partial", input.replay)
          }, 200)
        }
        return { grant: granted.grant, spans }
      }
    }))
  }
}
"#,
    )?;
    let host = Arc::new(GrantHost::default());
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = Arc::new(PluginSupervisor::with_host(
        runtime,
        DEFAULT_BRIDGE_LIMITS,
        Arc::clone(&host) as Arc<dyn PluginHost>,
    )?);
    let scratch = tempfile::tempdir()?;
    let resolved = resolve_declarations(&[ConfigDocument {
        directory: root.clone(),
        source: "explicit-fixture".into(),
        origin: PluginOrigin::LaunchExplicit,
        value: serde_json::json!({"plugins":[{"package":"./fixture.mjs","permissions":{"write":[scratch.path()]}}]}),
    }])?;
    assert!(resolved.blocked.is_empty(), "{:?}", resolved.blocked);
    let generation = gated_activate(&supervisor, resolved.active, serde_json::json!({}))?;
    let callback = first_tool(&generation, "fixture.grants", "slow")?;
    let replay = scratch.path().join("replay.txt");
    let calls = ["call_one", "call_two"]
        .into_iter()
        .map(|call| {
            let supervisor = Arc::clone(&supervisor);
            let id = generation.id();
            let replay = replay.to_string_lossy().into_owned();
            thread::spawn(move || {
                supervisor.invoke(
                    id,
                    "fixture.grants",
                    callback,
                    serde_json::json!({"replay": (call == "call_two").then_some(replay)}),
                    serde_json::json!({}),
                    scoped(call),
                )
            })
        })
        .collect::<Vec<_>>();
    let mut results = Vec::new();
    for call in calls {
        results.push(call.join().map_err(|_| "grant call panicked")??);
    }
    let grants = results
        .iter()
        .map(|result| result["grant"].as_u64().ok_or("grant id"))
        .collect::<Result<Vec<_>, _>>()?;
    assert_ne!(grants[0], grants[1], "two calls shared one grant");
    let spans = results
        .iter()
        .map(|result| result["spans"].as_array().map(Vec::len).unwrap_or(0))
        .max()
        .unwrap_or(0);
    assert_eq!(spans, 2);
    let final_spans = results
        .iter()
        .find(|result| {
            result["spans"]
                .as_array()
                .is_some_and(|spans| spans.len() == 2)
        })
        .ok_or("final spans")?["spans"]
        .clone();
    let first_end = final_spans[0][1].as_u64().ok_or("first end")?;
    let second_start = final_spans[1][0].as_u64().ok_or("second start")?;
    assert!(
        second_start >= first_end,
        "two top-level calls ran concurrently on one plugin process: {final_spans}"
    );
    let mut outcome = String::new();
    for _ in 0..100 {
        if let Ok(value) = std::fs::read_to_string(&replay) {
            outcome = value;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        outcome.contains("not live on this plugin process"),
        "a finished call's grant was replayed: {outcome:?}"
    );
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn explicit_cancellation_ends_only_its_call_and_an_ignored_cancel_is_bounded(
) -> Result<(), Box<dyn Error>> {
    let (root, honoring) = fixture(
        "cancel-honoring",
        r#"
export default {
  id: "fixture.honoring",
  async setup(ctx) {
    await ctx.tool.transform((draft) => {
      draft.add({ name: "wait", args: {type:"object"}, async execute(_input, context) {
        await new Promise((_resolve, reject) => {
          context.abort.addEventListener("abort", () => reject(new Error("aborted by host")), {once:true})
          ctx.session.get({}).catch(reject)
        })
      }})
      draft.add({ name: "ignore", args: {type:"object"}, async execute() {
        await ctx.session.get({})
        return new Promise(() => {})
      } })
    })
  }
}
"#,
    )?;
    let other = root.join("other").join("other.mjs");
    std::fs::create_dir_all(other.parent().ok_or("other parent")?)?;
    std::fs::write(
        &other,
        r#"
export default {
  id: "fixture.other",
  async setup(ctx) {
    await ctx.tool.transform((draft) => draft.add({ name: "steady", args: {type:"object"}, async execute() {
      await ctx.session.get({})
      await new Promise((resolve) => setTimeout(resolve, 600))
      return { output: "unaffected" }
    }}))
  }
}
"#,
    )?;
    let mut declared = declarations(&honoring)?;
    declared.extend(declarations(&other)?);
    let (runtime, _root_cleanup) = runtime()?;
    struct InvocationStarted(mpsc::SyncSender<String>);
    impl PluginHost for InvocationStarted {
        fn call(
            &self,
            context: &PluginHostContext,
            operation: HostOperation,
            _payload: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            if operation != HostOperation::SessionGet {
                return Err("unexpected cancellation fixture host operation".to_owned());
            }
            let call = context
                .grant()
                .and_then(|grant| grant.scope().downcast_ref::<String>())
                .ok_or("cancellation fixture requires a scoped call")?;
            self.0
                .try_send(call.clone())
                .map_err(|error| error.to_string())?;
            Ok(serde_json::Value::Null)
        }
    }
    let (started, ready) = mpsc::sync_channel(1);
    let admitted = |call: &str| -> Result<(), Box<dyn Error>> {
        assert_eq!(ready.recv_timeout(Duration::from_secs(5))?, call);
        Ok(())
    };
    let supervisor = Arc::new(PluginSupervisor::with_host(
        runtime,
        BridgeLimits {
            cleanup_millis: 300,
            ..DEFAULT_BRIDGE_LIMITS
        },
        Arc::new(InvocationStarted(started)),
    )?);
    let generation = gated_activate(&supervisor, declared, serde_json::json!({}))?;
    let wait = first_tool(&generation, "fixture.honoring", "wait")?;
    let ignore = first_tool(&generation, "fixture.honoring", "ignore")?;
    let steady = first_tool(&generation, "fixture.other", "steady")?;
    let id = generation.id();
    let spawn = |plugin: &'static str, callback: u64, call: &'static str| {
        let supervisor = Arc::clone(&supervisor);
        thread::spawn(move || {
            supervisor.invoke(
                id,
                plugin,
                callback,
                serde_json::json!({}),
                serde_json::json!({}),
                scoped(call),
            )
        })
    };
    let waiting = spawn("fixture.honoring", wait, "call_wait");
    // A real host callback proves entry; elapsed time cannot prove admission.
    admitted("call_wait")?;
    let unaffected = spawn("fixture.other", steady, "call_steady");
    admitted("call_steady")?;
    assert!(supervisor.cancel_call(id, "fixture.honoring", "call_wait")?);
    let cancelled = waiting.join().map_err(|_| "cancelled call panicked")?;
    assert!(
        matches!(&cancelled, Err(PluginError::Invocation(message)) if message.contains("aborted by host")),
        "the cancelled call did not settle as aborted: {cancelled:?}"
    );
    assert!(
        supervisor.current_is_running(id)?,
        "an honored per-call cancellation invalidated the whole generation"
    );
    let steady_result = unaffected.join().map_err(|_| "steady call panicked")?;
    assert!(
        matches!(&steady_result, Ok(value) if value["output"] == "unaffected"),
        "an honored per-call cancellation disturbed another plugin's call: {steady_result:?}"
    );

    let ignoring = spawn("fixture.honoring", ignore, "call_ignore");
    admitted("call_ignore")?;
    let bounded = supervisor.cancel_call(id, "fixture.honoring", "call_ignore");
    assert!(
        matches!(
            bounded,
            Err(PluginError::Timeout("plugin call cancellation"))
        ),
        "an ignored cancellation was not bounded: {bounded:?}"
    );
    let _ = ignoring.join();
    assert!(!supervisor.current_is_running(id)?);
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn reload_at_the_admitted_maximum_succeeds_and_one_more_plugin_is_refused_at_admission(
) -> Result<(), Box<dyn Error>> {
    let (root, _) = fixture(
        "reload-bound",
        "export default {id:'fixture.unused',setup(){}}
",
    )?;
    let mut declared = Vec::new();
    for index in 0..3 {
        let path = root.join(format!("plugin-{index}")).join("plugin.mjs");
        std::fs::create_dir_all(path.parent().ok_or("plugin parent")?)?;
        std::fs::write(
            &path,
            format!(
                "export default {{id:'fixture.bound.{index}',setup(){{}}}}
"
            ),
        )?;
        declared.push(declarations(&path)?);
    }
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::with_budget(
        runtime,
        DEFAULT_BRIDGE_LIMITS,
        SupervisorBudget {
            processes: 4,
            ..DEFAULT_SUPERVISOR_BUDGET
        },
        Arc::new(GrantHost::default()),
    )?;
    let maximum = declared[..2].concat();
    let first = gated_activate(&supervisor, maximum.clone(), serde_json::json!({}))?;
    for _ in 0..2 {
        let reloaded = gated_activate(&supervisor, maximum.clone(), serde_json::json!({}));
        assert!(
            matches!(&reloaded, Ok(generation) if generation.id() != first.id()),
            "a reload at the admitted maximum was refused: {reloaded:?}"
        );
    }
    let current = supervisor.current()?.ok_or("no published generation")?;
    let over = supervisor.prepare(declared.concat(), serde_json::json!({}));
    assert!(
        matches!(&over, Err(PluginError::Registration(message)) if message.contains("exceed the reload-safe bound of 2")),
        "one plugin over the admitted maximum was not refused at admission: {over:?}"
    );
    assert!(
        supervisor.current_is_running(current.id())?,
        "an admission refusal disturbed the published generation"
    );
    assert_eq!(supervisor.live_processes(), 2);
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_discarded_candidate_leaves_the_published_generation_serving() -> Result<(), Box<dyn Error>> {
    let (root, plugin) = fixture(
        "candidate",
        r#"
export default {
  id: "fixture.candidate",
  async setup(ctx) {
    await ctx.tool.transform((draft) => draft.add({ name: "echo", args: {type:"object"}, async execute() { return {output:"published"} } }))
  }
}
"#,
    )?;
    let (runtime, _root_cleanup) = runtime()?;
    let supervisor = PluginSupervisor::new(runtime, DEFAULT_BRIDGE_LIMITS)?;
    let declaration = declarations(&plugin)?;
    let published = gated_activate(&supervisor, declaration.clone(), serde_json::json!({}))?;
    let candidate = supervisor.prepare(declaration, serde_json::json!({}))?;
    assert_ne!(candidate.generation().id(), published.id());
    assert_eq!(
        supervisor.current()?.map(|generation| generation.id()),
        Some(published.id()),
        "a prepared candidate became visible before publication"
    );
    candidate.discard()?;
    let callback = first_tool(&published, "fixture.candidate", "echo")?;
    let output = supervisor.invoke(
        published.id(),
        "fixture.candidate",
        callback,
        serde_json::json!({}),
        serde_json::json!({}),
        CallAuthority::Unscoped,
    )?;
    assert_eq!(output["output"], "published");
    assert!(supervisor.current_is_running(published.id())?);
    supervisor.shutdown()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

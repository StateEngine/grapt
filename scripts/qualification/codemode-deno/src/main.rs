//! Isolated-process comparator. No product runtime or sandbox acceptance claim.
#[path = "../../codemode-runtime/src/framing.rs"]
mod framing;
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use deno_core::{v8, InspectorSessionKind, JsRuntime, JsRuntimeInspector, RuntimeOptions};
use serde_json::{json, Value};

const SOURCE_BYTES: usize = 65_536;
const OUTPUT_BYTES: usize = 65_536;
const HEAP_BYTES: usize = 16 * 1024 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (framed, source) = framing::source(SOURCE_BYTES)?;
    if source.len() > SOURCE_BYTES {
        return emit(json!({"result":{"status":"source_limit"}}));
    }
    let source = std::str::from_utf8(&source)?;
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let _entered = reactor.enter();
    let mut runtime = JsRuntime::try_new(RuntimeOptions {
        inspector: true,
        create_params: Some(v8::Isolate::create_params().heap_limits(0, HEAP_BYTES)),
        ..Default::default()
    })?;
    // Remove the embedding's own core operations before untrusted evaluation.
    runtime.execute_script("qualification-bootstrap", "delete globalThis.Deno;")?;
    framing::ready(framed)?;
    let program = if parses_as_module(&mut runtime, source)? {
        source.to_owned()
    } else {
        format!("await (async function(){{\n{source}\n}})()")
    };
    let reply = Rc::new(RefCell::new(None));
    let response = Rc::clone(&reply);
    let oversized = Rc::new(Cell::new(false));
    let overflow = Rc::clone(&oversized);
    let mut session = JsRuntimeInspector::create_local_session(
        runtime.inspector(),
        Box::new(move |message| {
            if message.content.len() > OUTPUT_BYTES * 6 + 4096 {
                overflow.set(true);
            } else if response.borrow().is_none() {
                *response.borrow_mut() = Some(message.content);
            }
        }),
        InspectorSessionKind::NonBlocking {
            wait_for_disconnect: false,
        },
    );
    session.post_message(
        1,
        "Runtime.evaluate",
        Some(json!({
            "expression":program, "replMode":true, "awaitPromise":true,
            "returnByValue":true, "timeout":100,
        })),
    );
    let mut task = Context::from_waker(Waker::noop());
    let mut polls = 0;
    while reply.borrow().is_none() && !oversized.get() && polls < 1024 {
        polls += 1;
        match runtime.poll_event_loop(&mut task, Default::default()) {
            Poll::Ready(Err(_)) => break,
            Poll::Ready(Ok(())) | Poll::Pending => {}
        }
    }
    let observed = if oversized.get() {
        json!({"status":"output_limit"})
    } else if let Some(reply) = reply.borrow_mut().take() {
        interpret(&reply)?
    } else {
        json!({"status":"unresolved_promise"})
    };
    emit(json!({"result":observed,"event_loop_polls":polls,
        "heap_limit":HEAP_BYTES,"external_memory_limit":"not established"}))
}

fn parses_as_module(
    runtime: &mut JsRuntime,
    source: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    deno_core::scope!(scope, runtime);
    v8::tc_scope!(let scope, scope);
    let code = v8::String::new(scope, source).ok_or("source allocation failed")?;
    let name = v8::String::new(scope, "qualification-parse").ok_or("name allocation failed")?;
    let origin = v8::ScriptOrigin::new(
        scope,
        name.into(),
        0,
        0,
        false,
        0,
        None,
        false,
        false,
        true,
        None,
    );
    let mut input = v8::script_compiler::Source::new(code, Some(&origin));
    Ok(v8::script_compiler::compile_module(scope, &mut input).is_some())
}

fn interpret(reply: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let reply: Value = serde_json::from_str(reply)?;
    let result = &reply["result"];
    if reply.get("error").is_some() || result.get("exceptionDetails").is_some() {
        return Ok(json!({"status":"guest_error"}));
    }
    let value = &result["result"];
    if value["type"] == "undefined" {
        return Ok(json!({"status":"completed","undefined":true}));
    }
    let text = serde_json::to_string(&json!({"value":value["value"]}))?;
    if text.len() > OUTPUT_BYTES {
        return Ok(json!({"status":"output_limit"}));
    }
    Ok(json!({"status":"completed","value_json":text}))
}

fn emit(value: Value) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

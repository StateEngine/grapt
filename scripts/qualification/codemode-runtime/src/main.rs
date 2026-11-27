//! Qualification only: no product authority or production runtime selection.
mod bridge;
mod framing;
use std::cell::Cell;
use std::io::Write;
use std::rc::Rc;

use rquickjs::{Context, Module, Runtime, Value};
use serde_json::json;

const SOURCE_BYTES: usize = 65_536;
const OUTPUT_BYTES: usize = 65_536;
const HEAP_BYTES: usize = 16 * 1024 * 1024;
const STACK_BYTES: usize = 256 * 1024;
const INTERRUPTS: usize = 1_000;
const JOBS: usize = 1_024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executions = if std::env::args().nth(1).as_deref() == Some("--batch") {
        3
    } else {
        1
    };
    for _ in 0..executions {
        execute()?;
    }
    Ok(())
}

fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let (framed, input) = framing::source(SOURCE_BYTES)?;
    if input.len() > SOURCE_BYTES {
        return emit(json!({"status":"source_limit"}));
    }
    let source = std::str::from_utf8(&input)?;
    // Module::declare registers the parsed module. Its context/runtime must
    // be gone before execution, otherwise dynamic import can execute it again.
    let program = select_program(source)?;
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(HEAP_BYTES);
    runtime.set_max_stack_size(STACK_BYTES);
    let visits = Rc::new(Cell::new(0_usize));
    let count = Rc::clone(&visits);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let next = count.get().saturating_add(1);
        count.set(next);
        next >= INTERRUPTS
    })));
    let context = Context::full(&runtime)?;
    framing::ready(framed)?;
    let result = context.with(|ctx| {
        // No libc, module loader or timers. Only the inert echo probe is installed.
        let bridge = match bridge::Bridge::new(&ctx) {
            Ok(bridge) => bridge,
            Err(_) => return json!({"status":"host_initialization_error"}),
        };
        let mut outcome = (|| {
            let promise = match ctx.eval_promise(program) {
            Ok(promise) => promise,
            Err(error) => {
                if visits.get() >= INTERRUPTS {
                    return json!({"status":"interrupted"});
                }
                return json!({"status":"parse_or_start_error", "error":error.to_string()});
            }
        };
        for jobs in 0..=JOBS {
            if let Some(result) = promise.result::<Value>() {
                let result = match result {
                    Ok(value) => match ctx.json_stringify(value) {
                        Ok(Some(text)) => match text.to_cstring() {
                            Ok(text) if text.len() <= OUTPUT_BYTES => {
                                json!({"status":"completed","value_json":text.as_str(),"jobs":jobs})
                            }
                            Ok(_) => json!({"status":"output_limit","jobs":jobs}),
                            Err(_) => json!({"status":"serialization_error","jobs":jobs}),
                        },
                        Ok(None) => json!({"status":"completed","undefined":true,"jobs":jobs}),
                        Err(_) => json!({"status":"serialization_error","jobs":jobs}),
                    },
                    Err(error) => {
                        json!({"status":"guest_error","error":error.to_string(),"jobs":jobs})
                    }
                };
                return result;
            }
            if jobs == JOBS {
                return json!({"status":"job_limit","jobs":jobs});
            }
            if !ctx.execute_pending_job() {
                if visits.get() >= INTERRUPTS {
                    return json!({"status":"interrupted","jobs":jobs});
                }
                if bridge::pending_barrier(bridge.pending_count()).is_err() {
                    return json!({"status":"host_cancelled","jobs":jobs});
                }
                match bridge.settle_one(&ctx) {
                    Ok(true) => continue,
                    Ok(false) => return json!({"status":"unresolved_promise","jobs":jobs}),
                    Err(_) => return json!({"status":"host_settlement_error","jobs":jobs}),
                }
            }
        }
        json!({"status":"job_limit"})
        })();
        if visits.get() >= INTERRUPTS {
            outcome["status"] = json!("interrupted");
        }
        outcome["host"] = bridge.receipt();
        outcome
    });
    emit(json!({"result":result,"interrupt_visits":visits.get(),
        "guest_memory_used":runtime.memory_usage().memory_used_size,
        "heap_limit":HEAP_BYTES,"stack_limit":STACK_BYTES}))
}

fn select_program(source: &str) -> Result<String, Box<dyn std::error::Error>> {
    let parser = Runtime::new()?;
    parser.set_memory_limit(HEAP_BYTES);
    parser.set_max_stack_size(STACK_BYTES);
    let context = Context::full(&parser)?;
    let module_syntax = context.with(|ctx| {
        let parsed = Module::declare(ctx.clone(), "qualification-parse", source).is_ok();
        if !parsed {
            let _parse_exception = ctx.catch();
        }
        parsed
    });
    Ok(if module_syntax {
        source.to_owned()
    } else {
        format!("await (async function(){{\n{source}\n}})()")
    })
}

fn emit(value: serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
    // JSON escaping can expand the already bounded output by up to six times.
    let text = serde_json::to_vec(&value)?;
    if text.len() > OUTPUT_BYTES * 6 + 4_096 {
        return Err("qualification receipt exceeded its bound".into());
    }
    let mut output = std::io::stdout().lock();
    output.write_all(&text)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

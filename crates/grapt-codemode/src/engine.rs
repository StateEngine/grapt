use std::cell::Cell;
use std::io::{Read, Write};
use std::rc::Rc;

use rquickjs::{Context, Module, Runtime, Value};

use crate::{
    bridge::Bridge, read_frame, write_frame, ExecuteRequest, GuestFailure, GuestResult,
    ProtocolError, WorkerMessage, HEAP_BYTES, INTERRUPT_VISITS, JOBS, OUTPUT_BYTES, STACK_BYTES,
};

/// One fresh guest on private pipes. EOF or malformed host frames cannot retry
/// a call. The parent owns elapsed timeout, cancellation, effects and quarantine.
pub fn run_worker<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> Result<(), ProtocolError> {
    let request: ExecuteRequest = read_frame(reader)?;
    request.validate()?;
    let result = execute(&request, reader, writer)?;
    write_frame(writer, &WorkerMessage::Finished { result })
}

fn runtime() -> Result<Runtime, ProtocolError> {
    let runtime = Runtime::new().map_err(|_| ProtocolError::Invalid("guest initialization"))?;
    runtime.set_memory_limit(HEAP_BYTES);
    runtime.set_max_stack_size(STACK_BYTES);
    Ok(runtime)
}

fn program(source: &str) -> Result<String, ProtocolError> {
    // Module::declare registers its module. Discard this entire runtime before
    // execution so dynamic import cannot execute the parsing probe a second time.
    let parser = runtime()?;
    let context = Context::full(&parser).map_err(|_| ProtocolError::Invalid("parser context"))?;
    let module = context.with(|ctx| {
        let valid = Module::declare(ctx.clone(), "codemode-parse", source).is_ok();
        if !valid {
            let _exception = ctx.catch();
        }
        valid
    });
    Ok(if module {
        source.to_owned()
    } else {
        format!("await (async function(){{\n{source}\n}})()")
    })
}

fn failed(reason: GuestFailure) -> GuestResult {
    GuestResult::Failed {
        reason,
        detail: None,
        nested_failure: None,
    }
}

fn execute<R: Read, W: Write>(
    request: &ExecuteRequest,
    reader: &mut R,
    writer: &mut W,
) -> Result<GuestResult, ProtocolError> {
    let program = program(&request.code)?;
    let runtime = runtime()?;
    let visits = Rc::new(Cell::new(0_usize));
    let count = Rc::clone(&visits);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let next = count.get().saturating_add(1);
        count.set(next);
        next >= INTERRUPT_VISITS
    })));
    let context = Context::full(&runtime).map_err(|_| ProtocolError::Invalid("guest context"))?;
    let result = context.with(|ctx| -> Result<GuestResult, ProtocolError> {
        let bridge =
            Bridge::new(&ctx, request).map_err(|_| ProtocolError::Invalid("guest bridge"))?;
        let promise = match ctx.eval_promise(program) {
            Ok(promise) => promise,
            Err(_) => {
                return Ok(GuestResult::Failed {
                    reason: GuestFailure::InvalidSource,
                    detail: bridge.describe(ctx.catch()),
                    nested_failure: None,
                })
            }
        };
        for jobs in 0..=JOBS {
            if let Some(result) = promise.result::<Value>() {
                let value = match result {
                    Ok(value) => value,
                    Err(_) => {
                        let error = ctx.catch();
                        return Ok(if bridge.approval_failure(error.clone()) {
                            failed(GuestFailure::ApprovalUnavailable)
                        } else {
                            let nested_failure = bridge.nested_failure(error.clone());
                            GuestResult::Failed {
                                reason: if nested_failure.is_some() {
                                    GuestFailure::GuestError
                                } else {
                                    bridge.error_kind(error.clone())
                                },
                                nested_failure,
                                detail: bridge.describe(error),
                            }
                        });
                    }
                };
                return match ctx.json_stringify(value) {
                    Ok(Some(text)) => {
                        let text = match text.to_cstring() {
                            Ok(text) => text,
                            Err(_) => return Ok(failed(GuestFailure::SerializationError)),
                        };
                        if text.len() > OUTPUT_BYTES {
                            return Ok(failed(GuestFailure::OutputLimit));
                        }
                        // eval_promise returns a wrapper whose optional value
                        // distinguishes JavaScript undefined from JSON null.
                        let value: serde_json::Value = serde_json::from_str(text.as_str())?;
                        Ok(GuestResult::Completed {
                            value: value.get("value").cloned(),
                        })
                    }
                    Ok(None) => Ok(GuestResult::Completed { value: None }),
                    Err(_) => Ok(failed(GuestFailure::SerializationError)),
                };
            }
            if jobs == JOBS {
                return Ok(failed(GuestFailure::JobLimit));
            }
            if !ctx.execute_pending_job() {
                if visits.get() >= INTERRUPT_VISITS {
                    return Ok(failed(GuestFailure::Interrupted));
                }
                if !bridge.exchange(&ctx, reader, writer)? {
                    return Ok(failed(GuestFailure::UnresolvedPromise));
                }
            }
        }
        Ok(failed(GuestFailure::JobLimit))
    })?;
    Ok(if visits.get() >= INTERRUPT_VISITS {
        failed(GuestFailure::Interrupted)
    } else {
        result
    })
}

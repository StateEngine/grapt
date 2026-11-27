//! Inert qualification host. Echo is an in-memory round trip, not a tool effect.
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::rc::Rc;

use rquickjs::{Ctx, Function, Object, Result, String as JsString, Value};

const CALLS: usize = 64;
const IN_FLIGHT: usize = 8;
const FRAME_BYTES: usize = 256 * 1024;
const QUEUE_BYTES: usize = 1024 * 1024;

pub fn pending_barrier(count: usize) -> std::io::Result<()> {
    if count > 0 && std::env::args().nth(1).as_deref() == Some("--pending") {
        let mut output = std::io::stdout().lock();
        writeln!(output, "PENDING {count}")?;
        output.flush()?;
        let mut release = [0];
        std::io::stdin().read_exact(&mut release)?;
        if release != [1] {
            return Err(std::io::Error::other("invalid pending barrier release"));
        }
    }
    Ok(())
}

pub struct Bridge<'js> {
    queue: Rc<RefCell<VecDeque<(usize, std::string::String)>>>,
    bytes: Rc<Cell<usize>>,
    admitted: Rc<Cell<usize>>,
    completed: Cell<usize>,
    settlement_failed: Cell<usize>,
    settle: Function<'js>,
}

impl<'js> Bridge<'js> {
    pub fn new(ctx: &Ctx<'js>) -> Result<Self> {
        let queue = Rc::new(RefCell::new(VecDeque::new()));
        let bytes = Rc::new(Cell::new(0_usize));
        let admitted = Rc::new(Cell::new(0_usize));
        let pending = Rc::clone(&queue);
        let pending_bytes = Rc::clone(&bytes);
        let total = Rc::clone(&admitted);
        let send = Function::new(ctx.clone(), move |input: JsString<'js>| -> Result<usize> {
            let input = input.to_cstring()?;
            let mut queue = pending.borrow_mut();
            if input.len() > FRAME_BYTES
                || queue.len() >= IN_FLIGHT
                || total.get() >= CALLS
                || pending_bytes.get().saturating_add(input.len()) > QUEUE_BYTES
            {
                return Err(rquickjs::Error::new_from_js_message(
                    "input",
                    "host admission",
                    "qualification host capacity exceeded",
                ));
            }
            let id = total.get();
            total.set(id + 1);
            pending_bytes.set(pending_bytes.get() + input.len());
            queue.push_back((id, input.as_str().to_owned()));
            Ok(id)
        })?;
        let create: Function = ctx.eval(
            "(send => { const pending = new Map(); const stringify = JSON.stringify; \
             const PromiseCtor = Promise; const get = pending.get.bind(pending); \
             const set = pending.set.bind(pending); const remove = pending.delete.bind(pending); \
             return {tools: Object.freeze({echo(input) { \
               const id = send(stringify(input)); \
               return new PromiseCtor((resolve) => set(id, resolve)); \
             }}), settle(id, value) { const resolve = get(id); \
               if (!resolve) throw new Error('unknown host result'); \
               remove(id); resolve(value); \
             }}; })",
        )?;
        let values: Object = create.call((send,))?;
        ctx.globals()
            .set("tools", values.get::<_, Object>("tools")?)?;
        Ok(Self {
            queue,
            bytes,
            admitted,
            completed: Cell::new(0),
            settlement_failed: Cell::new(0),
            settle: values.get("settle")?,
        })
    }

    pub fn settle_one(&self, ctx: &Ctx<'js>) -> Result<bool> {
        let Some((id, input)) = self.queue.borrow_mut().pop_front() else {
            return Ok(false);
        };
        self.bytes.set(self.bytes.get() - input.len());
        let settled = (|| {
            let value: Value = ctx.json_parse(input)?;
            self.settle.call::<_, ()>((id, value))
        })();
        if let Err(error) = settled {
            self.settlement_failed.set(self.settlement_failed.get() + 1);
            return Err(error);
        }
        self.completed.set(self.completed.get() + 1);
        Ok(true)
    }

    pub fn pending_count(&self) -> usize {
        self.queue.borrow().len()
    }

    pub fn receipt(&self) -> serde_json::Value {
        serde_json::json!({"admitted":self.admitted.get(), "completed":self.completed.get(),
            "cancelled_pending":self.queue.borrow().len(), "pending_bytes":self.bytes.get(),
            "settlement_failed":self.settlement_failed.get()})
    }
}

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::rc::Rc;

use grapt_types::ToolDefinitionId;
use rquickjs::{Ctx, Function, Object, String as JsString, Value};

use crate::{
    bounded_json, read_frame, write_frame, ExecuteRequest, HostCall, HostFailure, HostReply,
    HostResult, ProtocolError, WorkerMessage, ARGUMENT_BYTES, HOST_CALLS, IN_FLIGHT,
    QUEUED_ARGUMENT_BYTES,
};

pub(crate) struct Bridge<'js> {
    queue: Rc<RefCell<VecDeque<HostCall>>>,
    outstanding: Rc<RefCell<BTreeMap<u16, ToolDefinitionId>>>,
    queued_bytes: Rc<Cell<usize>>,
    settle: Function<'js>,
    is_approval_failure: Function<'js>,
    describe: Function<'js>,
    failure_reason: Function<'js>,
    error_kind: Function<'js>,
}

fn rejected() -> rquickjs::Error {
    rquickjs::Error::new_from_js_message(
        "request",
        "host call",
        "invalid or over-bound host request",
    )
}

impl<'js> Bridge<'js> {
    pub(crate) fn new(ctx: &Ctx<'js>, request: &ExecuteRequest) -> rquickjs::Result<Self> {
        let queue = Rc::new(RefCell::new(VecDeque::new()));
        let outstanding = Rc::new(RefCell::new(BTreeMap::new()));
        let queued_bytes = Rc::new(Cell::new(0_usize));
        let count = Cell::new(0_usize);
        let pending = Rc::clone(&queue);
        let active = Rc::clone(&outstanding);
        let bytes = Rc::clone(&queued_bytes);
        let catalog = request.catalog.clone();
        let allowed: BTreeSet<_> = request.tools.iter().map(|tool| tool.name.clone()).collect();
        let send = Function::new(
            ctx.clone(),
            move |name: JsString<'js>, input: JsString<'js>| -> rquickjs::Result<u16> {
                let name = name.to_cstring()?;
                let input = input.to_cstring()?;
                if name.len() > 256
                    || input.len() > ARGUMENT_BYTES
                    || count.get() >= HOST_CALLS
                    || active.borrow().len() >= IN_FLIGHT
                    || bytes.get().saturating_add(input.len()) > QUEUED_ARGUMENT_BYTES
                {
                    return Err(rejected());
                }
                let tool = ToolDefinitionId::new(name.as_str()).ok_or_else(rejected)?;
                if !allowed.contains(&tool) {
                    return Err(rejected());
                }
                let arguments: serde_json::Value =
                    serde_json::from_str(input.as_str()).map_err(|_| rejected())?;
                if !arguments.is_object() {
                    return Err(rejected());
                }
                let ordinal = u16::try_from(count.get()).map_err(|_| rejected())?;
                count.set(count.get() + 1);
                bytes.set(bytes.get() + input.len());
                active.borrow_mut().insert(ordinal, tool.clone());
                pending.borrow_mut().push_back(HostCall {
                    ordinal,
                    catalog: catalog.clone(),
                    tool,
                    arguments,
                });
                Ok(ordinal)
            },
        )?;
        let create: Function = ctx.eval(
            "((send,names)=>{const pending=new Map(); const get=pending.get.bind(pending); \
             const set=pending.set.bind(pending); const remove=pending.delete.bind(pending); \
             const approvals=new WeakSet(); const mark=approvals.add.bind(approvals); \
             const isApprovalFailure=approvals.has.bind(approvals); \
             const failures=new WeakMap(); const markFailure=failures.set.bind(failures); \
             const isFailure=failures.has.bind(failures); const failureData=failures.get.bind(failures); const failureReason=e=>failureData(e)?.reason; const S=String; const ErrorCtor=Error; const TypeErrorCtor=TypeError; const ReferenceErrorCtor=ReferenceError; const RangeErrorCtor=RangeError; \
             const own=Object.prototype.hasOwnProperty; const hasOwn=own.call.bind(own); const stringify=JSON.stringify; const PromiseCtor=Promise; const tools=Object.create(null); \
             for(const name of names) Object.defineProperty(tools,name,{enumerable:true,value(input){ \
               const ordinal=send(name,stringify(input)); \
               return new PromiseCtor((resolve,reject)=>set(ordinal,{resolve,reject})); \
             }}); return {tools:Object.freeze(tools),isApprovalFailure,failureReason,errorKind(e){if(e instanceof TypeErrorCtor)return 'type_error';if(e instanceof ReferenceErrorCtor)return 'reference_error';if(e instanceof RangeErrorCtor)return 'range_error';return 'guest_error';},settle(ordinal,ok,value){ \
               const entry=get(ordinal); if(!entry) throw new Error('unknown host reply'); \
               remove(ordinal); if(ok) entry.resolve(value); else { const message=hasOwn(value,'message')?value.message:undefined; markFailure(value,{reason:value.error,description:'tool '+S(value.tool)+' failed ('+S(value.error)+')'+(typeof message==='string'?': '+message:'')}); \
                 if(value.error==='approval_unavailable') mark(value); entry.reject(value); } \
             },describe(e){try{ \
               if(e!==null&&typeof e==='object'&&isFailure(e)) return failureData(e).description; \
               if(e instanceof ErrorCtor){const stack=typeof e.stack==='string'?e.stack:''; \
                 const at=stack.slice(0,stack.indexOf('\\n')<0?stack.length:stack.indexOf('\\n')).trim(); \
                 return S(e.name)+': '+S(e.message)+(at?' '+at:'');} \
               if(typeof e==='string') return e; const text=stringify(e); \
               return text===undefined?S(e):text; \
             }catch(_){return 'the thrown value could not be described';}}};})"
        )?;
        let names: Vec<_> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        let names = bounded_json(&names, crate::CATALOG_BYTES).map_err(|_| rejected())?;
        let names: Value = ctx.json_parse(names)?;
        let created: Object = create.call((send, names))?;
        ctx.globals()
            .set("tools", created.get::<_, Object>("tools")?)?;
        // Full schemas stay inside the existing bounded guest catalog until
        // the model explicitly returns one as an ordinary tool result.
        let catalog = bounded_json(&request.tools, crate::CATALOG_BYTES).map_err(|_| rejected())?;
        let catalog: Value = ctx.json_parse(catalog)?;
        let describe: Function = ctx.eval(include_str!("catalog.js"))?;
        let catalog: Object = describe.call((catalog,))?;
        ctx.globals().set("catalog", catalog)?;
        Ok(Self {
            queue,
            outstanding,
            queued_bytes,
            settle: created.get("settle")?,
            is_approval_failure: created.get("isApprovalFailure")?,
            describe: created.get("describe")?,
            failure_reason: created.get("failureReason")?,
            error_kind: created.get("errorKind")?,
        })
    }

    pub(crate) fn approval_failure(&self, value: Value<'js>) -> bool {
        self.is_approval_failure
            .call::<_, bool>((value,))
            .unwrap_or(false)
    }

    pub(crate) fn error_kind(&self, value: Value<'js>) -> crate::GuestFailure {
        let kind = self.error_kind.call::<_, String>((value,)).ok();
        match kind.as_deref() {
            Some("type_error") => crate::GuestFailure::TypeError,
            Some("reference_error") => crate::GuestFailure::ReferenceError,
            Some("range_error") => crate::GuestFailure::RangeError,
            _ => crate::GuestFailure::GuestError,
        }
    }

    pub(crate) fn nested_failure(&self, value: Value<'js>) -> Option<HostFailure> {
        let reason = self
            .failure_reason
            .call::<_, Option<String>>((value,))
            .ok()??;
        serde_json::from_value(serde_json::Value::String(reason)).ok()
    }

    /// What a thrown value says, bounded: an Error's name, message and first
    /// stack frame; a nested refusal's tool, reason and text; else the value.
    pub(crate) fn describe(&self, value: Value<'js>) -> Option<String> {
        let text = self.describe.call::<_, String>((value,)).ok()?;
        crate::bounded_detail(&text)
    }

    pub(crate) fn exchange<R: Read, W: Write>(
        &self,
        ctx: &Ctx<'js>,
        reader: &mut R,
        writer: &mut W,
    ) -> Result<bool, ProtocolError> {
        while let Some(call) = self.queue.borrow_mut().pop_front() {
            write_frame(writer, &WorkerMessage::Call { call })?;
        }
        self.queued_bytes.set(0);
        if self.outstanding.borrow().is_empty() {
            return Ok(false);
        }
        let reply: HostReply = read_frame(reader)?;
        let Some(tool) = self.outstanding.borrow_mut().remove(&reply.ordinal) else {
            return Err(ProtocolError::Invalid("host reply identity"));
        };
        let (ok, value) = match reply.result {
            HostResult::Completed { value } => (true, value),
            HostResult::Failed { reason, message } => {
                let mut error = serde_json::json!({"error":reason,"tool":tool.as_str()});
                if reason == crate::HostFailure::ApprovalUnavailable {
                    error["message"] = "approval unavailable (headless): no matching approver is attached; no effect ran".into();
                } else if let Some(message) = message.as_deref().and_then(crate::bounded_detail) {
                    error["message"] = message.into();
                }
                (false, error)
            }
        };
        let value = bounded_json(&value, ARGUMENT_BYTES)?;
        let value: Value = ctx
            .json_parse(value)
            .map_err(|_| ProtocolError::Invalid("host reply value"))?;
        self.settle
            .call::<_, ()>((reply.ordinal, ok, value))
            .map_err(|_| ProtocolError::Invalid("host reply settlement"))?;
        Ok(true)
    }
}

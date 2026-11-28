//! Nested effects use the same permits and runner, after the durable owner
//! acknowledges admission. Parent futures retain child futures; no detached job.
use super::*;
use grapt_types::{ToolDefinitionId, ToolSchemaVersion};
use tokio::sync::oneshot;

pub(super) const NESTED_CALL_LIMIT: usize = 64;
pub(super) const NESTED_CHANNEL_CAPACITY: usize = 64;
pub(super) const COMPOSITION_LIMIT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NestedCallError {
    Capacity,
    Closed,
    Duplicate,
    Recursion,
    Durability,
}

#[derive(Clone)]
pub struct NestedTools {
    parent: ToolCallId,
    attempt: PhysicalAttemptId,
    tools: Arc<dyn ToolRunner>,
    tx: mpsc::Sender<NestedRequest>,
    permits: Arc<Semaphore>,
    compositions: Arc<Semaphore>,
}

impl std::fmt::Debug for NestedTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NestedTools")
            .field("parent", &self.parent)
            .finish_non_exhaustive()
    }
}

impl NestedTools {
    pub(super) fn new(
        parent: ToolCallId,
        attempt: PhysicalAttemptId,
        tools: Arc<dyn ToolRunner>,
        tx: mpsc::Sender<NestedRequest>,
        permits: Arc<Semaphore>,
        compositions: Arc<Semaphore>,
    ) -> Self {
        Self {
            parent,
            attempt,
            tools,
            tx,
            permits,
            compositions,
        }
    }
    pub(super) fn composition_permits(&self) -> Arc<Semaphore> {
        Arc::clone(&self.compositions)
    }

    /// Call identities come from the product's injected ID authority. The
    /// handle fixes the parent and attempt; guest input cannot replace either.
    pub async fn call(
        &self,
        call: ToolCallId,
        tool: ToolDefinitionId,
        catalog: ToolSchemaVersion,
        input: BoundedToolInput,
    ) -> Result<ToolOutcome, NestedCallError> {
        if self.tools.is_composition(tool.as_str()) {
            return Err(NestedCallError::Recursion);
        }
        let _permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| NestedCallError::Closed)?;
        let (reply, admitted) = oneshot::channel();
        self.tx
            .send(NestedRequest::Start {
                parent: self.parent.clone(),
                call: call.clone(),
                tool: tool.clone(),
                catalog,
                input: input.clone(),
                reply,
            })
            .await
            .map_err(|_| NestedCallError::Closed)?;
        admitted.await.map_err(|_| NestedCallError::Closed)??;
        let mut custody = PendingNested {
            tx: self.tx.clone(),
            call: call.clone(),
            armed: true,
        };
        let outcome = self
            .tools
            .run_nested(
                Some(self.attempt.clone()),
                call.clone(),
                tool.as_str().to_owned(),
                input,
            )
            .await;
        let (reply, committed) = oneshot::channel();
        self.tx
            .send(NestedRequest::Terminal {
                call,
                outcome: outcome.clone(),
                reply,
            })
            .await
            .map_err(|_| NestedCallError::Closed)?;
        committed.await.map_err(|_| NestedCallError::Durability)??;
        custody.armed = false;
        Ok(outcome)
    }
}

struct PendingNested {
    tx: mpsc::Sender<NestedRequest>,
    call: ToolCallId,
    armed: bool,
}
impl Drop for PendingNested {
    fn drop(&mut self) {
        if self.armed {
            // Parent closure also scans its open calls. A full/closed channel
            // cannot erase this obligation or turn a missing result successful.
            let _sent = self.tx.try_send(NestedRequest::Abandoned {
                call: self.call.clone(),
            });
        }
    }
}

pub(super) enum NestedRequest {
    Start {
        parent: ToolCallId,
        call: ToolCallId,
        tool: ToolDefinitionId,
        catalog: ToolSchemaVersion,
        input: BoundedToolInput,
        reply: oneshot::Sender<Result<(), NestedCallError>>,
    },
    Terminal {
        call: ToolCallId,
        outcome: ToolOutcome,
        reply: oneshot::Sender<Result<(), NestedCallError>>,
    },
    Abandoned {
        call: ToolCallId,
    },
}

#[derive(Default)]
pub(super) struct NestedState {
    parents: std::collections::BTreeSet<ToolCallId>,
    seen: std::collections::BTreeSet<ToolCallId>,
    open: BTreeMap<ToolCallId, ToolCallId>,
}

impl NestedState {
    pub(super) fn register(&mut self, parent: ToolCallId) {
        self.parents.insert(parent);
    }
    pub(super) fn stop_admission(&mut self) {
        self.parents.clear();
    }
    pub(super) fn contains(&self, call: &ToolCallId) -> bool {
        self.seen.contains(call)
    }
    pub(super) fn handle<O: AttemptOwner>(
        &mut self,
        request: NestedRequest,
        owner: &mut O,
        top_calls: &BTreeMap<String, ToolCallId>,
    ) -> Result<(), OwnerError> {
        match request {
            NestedRequest::Start {
                parent,
                call,
                tool,
                catalog,
                input,
                reply,
            } => {
                let rejection = if !self.parents.contains(&parent) {
                    Some(NestedCallError::Closed)
                } else if self.seen.contains(&call) || top_calls.contains_key(call.as_str()) {
                    Some(NestedCallError::Duplicate)
                } else if self.seen.len() >= NESTED_CALL_LIMIT {
                    Some(NestedCallError::Capacity)
                } else {
                    None
                };
                if let Some(error) = rejection {
                    let _sent = reply.send(Err(error));
                    return Ok(());
                }
                if let Err(error) = commit(
                    owner,
                    DurableFact::NestedToolCalled {
                        parent: parent.clone(),
                        call: call.clone(),
                        tool,
                        catalog,
                        input,
                    },
                ) {
                    let _sent = reply.send(Err(NestedCallError::Durability));
                    return Err(error);
                }
                self.seen.insert(call.clone());
                self.open.insert(call, parent);
                let _sent = reply.send(Ok(()));
            }
            NestedRequest::Terminal {
                call,
                outcome,
                reply,
            } => {
                if !self.open.contains_key(&call) {
                    let _sent = reply.send(Err(NestedCallError::Closed));
                    return Ok(());
                }
                if let Err(error) = commit(
                    owner,
                    DurableFact::ToolTerminal {
                        call: call.clone(),
                        outcome,
                    },
                ) {
                    let _sent = reply.send(Err(NestedCallError::Durability));
                    return Err(error);
                }
                self.open.remove(&call);
                let _sent = reply.send(Ok(()));
            }
            NestedRequest::Abandoned { call } => self.close_call(owner, &call)?,
        }
        Ok(())
    }
    fn close_call<O: AttemptOwner>(
        &mut self,
        owner: &mut O,
        call: &ToolCallId,
    ) -> Result<(), OwnerError> {
        if self.open.contains_key(call) {
            let detail = match owner.capture_diagnostic_detail(
                "nested tool execution was abandoned before producing a terminal result; external effect state is unknown",
            ) {
                Ok(detail) => Some(detail),
                Err(error) => {
                    grapt_diagnostics::report(format!(
                        "nested failure diagnostic detail capture degraded for call {}: {}",
                        call.as_str(),
                        error.reason
                    ));
                    None
                }
            };
            commit(
                owner,
                DurableFact::ToolTerminal {
                    call: call.clone(),
                    outcome: ToolOutcome::Failed {
                        reason: ToolFailureReason::EffectUnknown,
                        detail,
                    },
                },
            )?;
            self.open.remove(call);
        }
        Ok(())
    }
    pub(super) fn close_parent<O: AttemptOwner>(
        &mut self,
        owner: &mut O,
        parent: &ToolCallId,
    ) -> Result<(), OwnerError> {
        self.parents.remove(parent);
        let calls: Vec<_> = self
            .open
            .iter()
            .filter(|(_, p)| *p == parent)
            .map(|(call, _)| call.clone())
            .collect();
        for call in calls {
            self.close_call(owner, &call)?;
        }
        Ok(())
    }
    pub(super) fn close_all<O: AttemptOwner>(&mut self, owner: &mut O) -> Result<(), OwnerError> {
        self.parents.clear();
        let calls: Vec<_> = self.open.keys().cloned().collect();
        for call in calls {
            self.close_call(owner, &call)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

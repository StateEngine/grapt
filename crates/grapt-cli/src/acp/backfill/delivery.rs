//! Successful connection-local writes, never attempted writes or server effects.
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::RpcFault;

pub(super) type TextKey = (String, &'static str, u64);
const MAX_DELIVERIES: usize = 10_000;

#[derive(Debug, Default)]
struct Prefix {
    bytes: usize,
    hash: Sha256,
}

#[derive(Debug, Default)]
pub(crate) struct Delivery {
    gap: bool,
    text: BTreeMap<TextKey, Prefix>,
    calls: BTreeSet<String>,
    terminals: BTreeSet<String>,
    synced: BTreeSet<String>,
}

impl Delivery {
    pub(crate) fn gap(&mut self) {
        self.gap = true;
    }
    pub(crate) fn had_gap(&self) -> bool {
        self.gap
    }

    pub(super) fn prefix(&self, key: &TextKey, full: &str) -> Result<usize, RpcFault> {
        let Some(sent) = self.text.get(key) else {
            return Ok(0);
        };
        let prefix = full.get(..sent.bytes).ok_or_else(|| {
            RpcFault::internal("delivered text prefix is outside a UTF-8 history boundary")
        })?;
        if Sha256::digest(prefix.as_bytes()) != sent.hash.clone().finalize() {
            return Err(RpcFault::internal(
                "delivered text prefix disagrees with durable history",
            ));
        }
        Ok(sent.bytes)
    }

    pub(crate) fn live_key(
        data: &BTreeMap<String, Value>,
        kind: &'static str,
    ) -> Result<TextKey, RpcFault> {
        let message = data
            .get("assistantMessageID")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFault::internal("live text identity is malformed"))?;
        let ordinal = data
            .get("ordinal")
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcFault::internal("live text ordinal is malformed"))?;
        Ok((message.to_owned(), kind, ordinal))
    }

    pub(crate) async fn send_text(
        &mut self,
        key: TextKey,
        delta: &str,
        write: impl std::future::Future<Output = Result<(), RpcFault>>,
    ) -> Result<(), RpcFault> {
        write.await?;
        self.record_text(key, delta)
    }

    pub(crate) fn record_text(&mut self, key: TextKey, delta: &str) -> Result<(), RpcFault> {
        self.reserve(!self.text.contains_key(&key))?;
        let prefix = self.text.entry(key).or_default();
        prefix.bytes = prefix
            .bytes
            .checked_add(delta.len())
            .ok_or_else(|| RpcFault::internal("delivered text length exhausted"))?;
        prefix.hash.update(delta.as_bytes());
        Ok(())
    }

    pub(crate) fn call_sent(&self, call: &str) -> bool {
        self.calls.contains(call)
    }
    pub(crate) fn terminal_sent(&self, call: &str) -> bool {
        self.terminals.contains(call)
    }

    pub(crate) fn record_call(&mut self, call: &str) -> Result<(), RpcFault> {
        self.reserve(!self.calls.contains(call))?;
        self.calls.insert(call.to_owned());
        Ok(())
    }
    pub(crate) fn record_terminal(&mut self, call: &str) -> Result<(), RpcFault> {
        self.reserve(!self.terminals.contains(call))?;
        self.terminals.insert(call.to_owned());
        Ok(())
    }
    // Claim before issuing a client file effect: even a partial refusal must
    // not repeat a previously written file on another terminal observation.
    pub(crate) fn claim_sync(&mut self, call: &str) -> Result<bool, RpcFault> {
        self.reserve(!self.synced.contains(call))?;
        Ok(self.synced.insert(call.to_owned()))
    }
    fn reserve(&self, new: bool) -> Result<(), RpcFault> {
        if new
            && self.text.len() + self.calls.len() + self.terminals.len() + self.synced.len()
                >= MAX_DELIVERIES
        {
            return Err(RpcFault::internal("ACP delivery ledger bound exceeded"));
        }
        Ok(())
    }
}

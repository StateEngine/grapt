// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: compare bounded hashes of the final serialized request,
// retaining no prompt text.
//! Request comparisons never certify provider cache reuse.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{self, Write};

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_ITEMS: usize = grapt_types::request_cache::MAX_PREFIX_ITEMS;
type Hash = [u8; 32];

/// A local difference from the previous observed request in this Session.
/// A stable request can still miss the provider's cache for an unreported reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDelta {
    FirstObservation,
    TargetChanged,
    CacheKeyChanged,
    RoutingChanged,
    ToolsChanged,
    InstructionsChanged,
    OptionsChanged,
    InputChanged { index: usize },
    InputShortened { index: usize },
    AppendOnly,
    Identical,
    ComparisonBound,
}

/// Debug deliberately omits hashes; durable snapshots contain hashes only.
#[derive(Clone)]
pub struct RequestFingerprint {
    target: Hash,
    cache_key: Hash,
    routing: Hash,
    tools: Hash,
    instructions: Hash,
    options: Hash,
    input: Hash,
    items: Vec<Hash>,
    count: usize,
    /// Actual compact JSON bytes, not tokens or billing.
    pub body_bytes: usize,
    pub tools_bytes: usize,
    pub instructions_bytes: usize,
}
impl std::fmt::Debug for RequestFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestFingerprint").finish_non_exhaustive()
    }
}
impl RequestFingerprint {
    pub(crate) fn of(body: &Value, target: &[&str], body_bytes: usize) -> Option<Self> {
        if body_bytes > MAX_BYTES {
            return None;
        }
        let object = body.as_object()?;
        let input_key = if object.contains_key("input") {
            "input"
        } else {
            "messages"
        };
        let input = object.get(input_key)?.as_array()?;
        let tools = hash_fields(object, ["tools"])?;
        // Top-level instructions precede the input array. System/developer
        // input items remain in its digest and prefix ladder: appending one
        // must not claim an edit to bytes before the stable input prefix.
        let instructions = hash_fields(object, ["instructions", "system"])?.0;
        let bytes = |key| -> Option<usize> {
            object
                .get(key)
                .map_or(Some(0), |value| hash(value).map(|hashed| hashed.1))
        };
        let mut instructions_bytes = bytes("instructions")?.checked_add(bytes("system")?)?;
        for item in input {
            if matches!(
                item.get("role").and_then(Value::as_str),
                Some("system" | "developer")
            ) {
                instructions_bytes = instructions_bytes.checked_add(hash(item)?.1)?;
            }
        }
        // New wire fields participate automatically: no silent comparison hole
        // when another route adds an option. The route's object is bounded.
        if object.len() > 64 {
            return None;
        }
        let options = object.keys().map(String::as_str).filter(|key| {
            ![
                "model",
                "prompt_cache_key",
                "tools",
                "instructions",
                "system",
                input_key,
            ]
            .contains(key)
        });
        Some(Self {
            target: hash_target(target, object.get("model"))?,
            cache_key: hash_fields(object, ["prompt_cache_key"])?.0,
            routing: hash_routing(None, None)?,
            tools: tools.0,
            instructions,
            options: hash_fields(object, options)?.0,
            input: hash(object.get(input_key)?)?.0,
            items: input
                .iter()
                .take(MAX_ITEMS)
                .map(|item| hash(item).map(|hashed| hashed.0))
                .collect::<Option<_>>()?,
            count: input.len(),
            body_bytes,
            tools_bytes: bytes("tools")?,
            instructions_bytes,
        })
    }
    /// Include only cache-affinity headers, never authorization credentials.
    pub(crate) fn with_routing(
        mut self,
        session: Option<&str>,
        turn: Option<&str>,
    ) -> Option<Self> {
        self.routing = hash_routing(session, turn)?;
        Some(self)
    }
    /// Snapshot the final staged wire request without retaining its text.
    pub fn durable(&self) -> Option<grapt_types::request_cache::RequestCacheFingerprint> {
        use grapt_types::request_cache::{RequestCacheFingerprint, RequestComponents};
        let mut chain = Sha256::new();
        chain.update(b"secode.request-prefix.v1\0");
        let prefixes = self
            .items
            .iter()
            .map(|item| {
                chain.update(item);
                chain.clone().finalize().into()
            })
            .collect();
        RequestCacheFingerprint::new(
            RequestComponents {
                target: self.target,
                cache_key: self.cache_key,
                routing: self.routing,
                tools: self.tools,
                instructions: self.instructions,
                options: self.options,
                input: self.input,
            },
            prefixes,
            u32::try_from(self.count).ok()?,
        )
    }
    pub fn compare(&self, previous: &Self) -> RequestDelta {
        if self.target != previous.target {
            return RequestDelta::TargetChanged;
        }
        if self.cache_key != previous.cache_key {
            return RequestDelta::CacheKeyChanged;
        }
        if self.routing != previous.routing {
            return RequestDelta::RoutingChanged;
        }
        if self.tools != previous.tools {
            return RequestDelta::ToolsChanged;
        }
        if self.instructions != previous.instructions {
            return RequestDelta::InstructionsChanged;
        }
        if self.options != previous.options {
            return RequestDelta::OptionsChanged;
        }
        for (index, (old, new)) in previous.items.iter().zip(&self.items).enumerate() {
            if old != new {
                return RequestDelta::InputChanged { index };
            }
        }
        if self.count < previous.count {
            return if self.count <= MAX_ITEMS {
                RequestDelta::InputShortened { index: self.count }
            } else {
                RequestDelta::ComparisonBound
            };
        }
        if self.input == previous.input {
            return RequestDelta::Identical;
        }
        if previous.count <= MAX_ITEMS && self.count > previous.count {
            RequestDelta::AppendOnly
        } else {
            RequestDelta::ComparisonBound
        }
    }
}
#[derive(Default)]
struct Hasher {
    hash: Sha256,
    bytes: usize,
}
impl Write for Hasher {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let total = self
            .bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= MAX_BYTES)
            .ok_or_else(|| io::Error::other("request diagnostic bound"))?;
        self.hash.update(bytes);
        self.bytes = total;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn hash(value: &Value) -> Option<(Hash, usize)> {
    let mut writer = Hasher::default();
    serde_json::to_writer(&mut writer, value).ok()?;
    Some((writer.hash.finalize().into(), writer.bytes))
}
fn hash_fields<'a>(
    object: &serde_json::Map<String, Value>,
    keys: impl IntoIterator<Item = &'a str>,
) -> Option<(Hash, usize)> {
    let mut writer = Hasher::default();
    writer.write_all(b"[").ok()?;
    for (index, key) in keys.into_iter().enumerate() {
        if index != 0 {
            writer.write_all(b",").ok()?;
        }
        writer.write_all(b"[").ok()?;
        serde_json::to_writer(&mut writer, key).ok()?;
        writer.write_all(b",").ok()?;
        serde_json::to_writer(&mut writer, &object.contains_key(key)).ok()?;
        writer.write_all(b",").ok()?;
        serde_json::to_writer(&mut writer, &object.get(key)).ok()?;
        writer.write_all(b"]").ok()?;
    }
    writer.write_all(b"]").ok()?;
    Some((writer.hash.finalize().into(), writer.bytes))
}
fn hash_routing(session: Option<&str>, turn: Option<&str>) -> Option<Hash> {
    let mut writer = Hasher::default();
    serde_json::to_writer(&mut writer, &(session, turn)).ok()?;
    Some(writer.hash.finalize().into())
}
fn hash_target(target: &[&str], model: Option<&Value>) -> Option<Hash> {
    let mut writer = Hasher::default();
    writer.write_all(b"[").ok()?;
    serde_json::to_writer(&mut writer, target).ok()?;
    writer.write_all(b",").ok()?;
    serde_json::to_writer(&mut writer, &model).ok()?;
    writer.write_all(b"]").ok()?;
    Some(writer.hash.finalize().into())
}
#[cfg(test)]
mod tests;

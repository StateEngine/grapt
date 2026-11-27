//! Verified launcher packets, read once per delivery in existing Session custody.
use super::ContinuityUnavailable as E;
use grapt_types::{Secret, SessionId};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

const MAX_PACKET: usize = 16_384;
const MAX_POINTER: usize = 16_384;
const MAX_SESSIONS: usize = 128;
const MAX_RETAINED: usize = 1_048_576;

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pointer {
    state: String,
    path: String,
    bytes: usize,
    sha256: String,
}

impl std::fmt::Debug for Pointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PacketPointer")
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl Pointer {
    pub(super) fn parse(raw: &str) -> Result<Self, E> {
        if raw.len() > MAX_POINTER {
            return Err(E::PacketInvalid);
        }
        let value: Self = serde_json::from_str(raw).map_err(|_| E::PacketInvalid)?;
        if value.state != "written"
            || value.bytes == 0
            || value.bytes > MAX_PACKET
            || value.path.len() > 8_192
            || value.path.chars().any(char::is_control)
            || !Path::new(&value.path).is_absolute()
            || value.sha256.len() != 64
            || !value
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(E::PacketInvalid);
        }
        Ok(value)
    }
}

pub(super) struct Verified {
    pub(super) pointer: Pointer,
    boot: Boot,
    text: Secret<String>,
}

enum Boot {
    Packet(String),
    Identity(identity::Identity),
}

impl std::fmt::Debug for Verified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedLaunchPacket")
            .finish_non_exhaustive()
    }
}

impl Verified {
    pub(super) fn id(&self) -> Option<&str> {
        match &self.boot {
            Boot::Packet(id) => Some(id),
            Boot::Identity(_) => None,
        }
    }
    pub(super) fn identity_fingerprint(
        &self,
        project: &str,
        agent: &str,
    ) -> Result<Option<&str>, E> {
        match &self.boot {
            Boot::Packet(_) => Ok(None),
            Boot::Identity(identity) => identity.fingerprint(project, agent).map(Some),
        }
    }
    pub(super) fn text(&self) -> &str {
        self.text.expose()
    }

    pub(super) fn require_acknowledgement(
        &self,
        reply: &Value,
        project: &str,
        agent: &str,
        session: &SessionId,
    ) -> Result<(), E> {
        acknowledgement::verify(self, reply, project, agent, session)
    }
}

fn read(pointer: &Pointer) -> Result<Verified, E> {
    let metadata = std::fs::symlink_metadata(&pointer.path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            E::PacketMissing
        } else {
            E::PacketUnreadable
        }
    })?;
    if !metadata.is_file() {
        return Err(E::PacketUnreadable);
    }
    if metadata.len() != pointer.bytes as u64 {
        return Err(E::PacketInvalid);
    }
    let file = File::open(&pointer.path).map_err(|_| E::PacketUnreadable)?;
    let mut bytes = Vec::with_capacity(pointer.bytes + 1);
    file.take((pointer.bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| E::PacketUnreadable)?;
    if bytes.len() != pointer.bytes {
        return Err(E::PacketInvalid);
    }
    let actual: String = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != pointer.sha256 {
        return Err(E::PacketInvalid);
    }
    let text = String::from_utf8(bytes).map_err(|_| E::PacketInvalid)?;
    let mut identifiers = text
        .lines()
        .filter_map(|line| line.strip_prefix("packet_id:").map(str::trim));
    let boot = match identifiers.next() {
        Some(id) => {
            if id.is_empty()
                || id.len() > 128
                || identifiers.next().is_some()
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(E::PacketInvalid);
            }
            Boot::Packet(id.to_owned())
        }
        None => Boot::Identity(identity::Identity::parse(&text)?),
    };
    Ok(Verified {
        pointer: pointer.clone(),
        boot,
        text: Secret::new(text),
    })
}

#[derive(Debug)]
struct Entry {
    binding: Secret<String>,
    pointer: Pointer,
    result: Result<Arc<Verified>, E>,
    reserved: usize,
}

#[derive(Debug, Default)]
pub(in crate::dogfood) struct Cache {
    entries: BTreeMap<SessionId, Entry>,
    reserved: usize,
}

impl Cache {
    pub(in crate::dogfood) fn remove(&mut self, session: &SessionId) {
        if let Some(old) = self.entries.remove(session) {
            self.reserved -= old.reserved;
        }
    }

    pub(super) fn load(
        &mut self,
        session: &SessionId,
        binding: &str,
        pointer: &Pointer,
    ) -> Result<Arc<Verified>, E> {
        self.load_with(session, binding, pointer, read)
    }

    fn load_with(
        &mut self,
        session: &SessionId,
        binding: &str,
        pointer: &Pointer,
        read: impl FnOnce(&Pointer) -> Result<Verified, E>,
    ) -> Result<Arc<Verified>, E> {
        let prior = self.entries.get(session);
        if let Some(prior) =
            prior.filter(|prior| prior.binding.expose() == binding && prior.pointer == *pointer)
        {
            return prior.result.clone();
        }
        // Admit the maximum retained cost before opening a file. Failed reads
        // are cached too: repairing delivery requires a new pointer or rebind.
        // Entry and Verified each retain a pointer; reserve both copies and
        // the maximum parsed boot identity before a successful read can allocate.
        let reserved = 2 * (pointer.path.len() + pointer.sha256.len() + pointer.state.len())
            + pointer.bytes
            + binding.len()
            + 576;
        let total = self.reserved - prior.map_or(0, |entry| entry.reserved) + reserved;
        if (prior.is_none() && self.entries.len() >= MAX_SESSIONS) || total > MAX_RETAINED {
            return Err(E::Bounds);
        }
        let result = read(pointer).map(Arc::new);
        self.entries.insert(
            session.clone(),
            Entry {
                binding: Secret::new(binding.to_owned()),
                pointer: pointer.clone(),
                result: result.clone(),
                reserved,
            },
        );
        self.reserved = total;
        result
    }
}

#[cfg(test)]
pub(super) mod tests;

mod acknowledgement;
mod identity;

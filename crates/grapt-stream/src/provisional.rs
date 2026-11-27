//! Live blocks a snapshot may show. A publish takes the subscriber lock, then
//! this registry's, and holds both from before its frame's offer through the
//! change: one linearization point. Readers and discards take only this lock,
//! never one a writer holds across a commit. Keyed by identity, a block leaves
//! only at its own (durable) end.
use crate::limits::SseError;
use grapt_types::{MessageId, SessionId};

/// The most live blocks one session's snapshot carries: 16 held finished
/// reasoning blocks plus one open text and one open reasoning.
pub const MAX_PROVISIONAL_BLOCKS: usize = 18;
/// The registry holds exactly what one snapshot may carry.
const MAX_OPEN_PER_SESSION: usize = MAX_PROVISIONAL_BLOCKS;

/// Which kind of live block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionalKind {
    Text,
    Reasoning,
}

/// One live block's full identity, as its frames name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionalBlock {
    pub message: MessageId,
    pub kind: ProvisionalKind,
    pub ordinal: u64,
}

/// One live block; `bytes` keeps offsets exact once `text` outgrew a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionalText {
    pub block: ProvisionalBlock,
    pub text: Option<String>,
    pub bytes: u64,
}

/// A change to one block, applied with the frame that carries it.
#[derive(Debug, Clone, Copy)]
pub enum ProvisionalChange<'a> {
    Open {
        session: &'a SessionId,
        block: &'a ProvisionalBlock,
    },
    Append {
        session: &'a SessionId,
        block: &'a ProvisionalBlock,
        text: &'a str,
    },
    /// The block's end was offered, or the block was refused or discarded.
    Close {
        session: &'a SessionId,
        block: &'a ProvisionalBlock,
    },
}

#[derive(Debug, Default)]
struct SessionBlocks {
    blocks: Vec<ProvisionalText>,
    /// Live blocks with no room here, by identity; only their own close clears one.
    omitted: Vec<ProvisionalBlock>,
    /// More omitted blocks than the producer's bound: never complete again.
    saturated: bool,
}

#[derive(Debug, Default)]
pub struct ProvisionalRegistry {
    sessions: std::collections::BTreeMap<SessionId, SessionBlocks>,
}

impl ProvisionalRegistry {
    /// Apply one change; an append returns the block's offset before it.
    pub fn apply(&mut self, change: ProvisionalChange<'_>, max_text: usize) -> Option<u64> {
        match change {
            ProvisionalChange::Open { session, block } => {
                let held = self.sessions.entry(session.clone()).or_default();
                if held.omitted.contains(block) || held.blocks.iter().any(|o| o.block == *block) {
                    return None;
                }
                if held.blocks.len() < MAX_OPEN_PER_SESSION {
                    held.blocks.push(ProvisionalText {
                        block: block.clone(),
                        text: Some(String::new()),
                        bytes: 0,
                    });
                } else if held.omitted.len() < MAX_OPEN_PER_SESSION {
                    held.omitted.push(block.clone());
                } else {
                    held.saturated = true;
                }
                None
            }
            ProvisionalChange::Append {
                session,
                block,
                text,
            } => {
                let held = self.sessions.get_mut(session)?;
                let open = held.blocks.iter_mut().find(|open| open.block == *block)?;
                let from = open.bytes;
                // An exhausted offset is withheld; text past one snapshot is dropped.
                open.bytes = from.checked_add(u64::try_from(text.len()).ok()?)?;
                open.text = open
                    .text
                    .take()
                    .filter(|kept| kept.len().saturating_add(text.len()) <= max_text)
                    .map(|mut kept| {
                        kept.push_str(text);
                        kept
                    });
                Some(from)
            }
            ProvisionalChange::Close { session, block } => {
                if let Some(held) = self.sessions.get_mut(session) {
                    held.blocks.retain(|open| open.block != *block);
                    held.omitted.retain(|omitted| omitted != block);
                    if held.blocks.is_empty() && held.omitted.is_empty() && !held.saturated {
                        self.sessions.remove(session);
                    }
                }
                None
            }
        }
    }
}

impl ProvisionalRegistry {
    /// The offset an append starts at, when its block is held and can advance.
    pub fn offset(&self, change: ProvisionalChange<'_>) -> Option<u64> {
        let ProvisionalChange::Append {
            session,
            block,
            text,
        } = change
        else {
            return None;
        };
        let held = self.sessions.get(session)?;
        let open = held.blocks.iter().find(|open| open.block == *block)?;
        open.bytes.checked_add(u64::try_from(text.len()).ok()?)?;
        Some(open.bytes)
    }

    /// The session's live blocks; incomplete while any is omitted or text-less.
    pub fn live(&self, session: &SessionId) -> Result<Vec<ProvisionalText>, SseError> {
        match self.sessions.get(session) {
            None => Ok(Vec::new()),
            Some(held)
                if held.saturated
                    || !held.omitted.is_empty()
                    || held.blocks.iter().any(|open| open.text.is_none()) =>
            {
                Err(SseError::RegistryUnavailable)
            }
            Some(held) => Ok(held.blocks.clone()),
        }
    }
}

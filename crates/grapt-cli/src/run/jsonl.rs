//! Snapshot once, then publish completed message upserts from durable pages.
//!
//! Completion-based JSONL publication.
//! Modified for Grapt's bounded durable reads and typed IDs.
//! Copyright 2025 OpenAI, Apache-2.0; license and NOTICE retained in licenses/openai-codex/.

use std::{collections::BTreeMap, io::Write};

use grapt_api::{
    u3a::{SessionMessageCompactionDto as Compaction, SessionMessageInfoDto as Message},
    SessionMessagesResponseDto,
};
use grapt_types::{MessageId, SessionId};
use serde::Serialize;

use super::RunError;
use crate::{ApiClientError, BoundedOutput};

#[derive(Default)]
pub(super) struct JsonlMessages {
    initialized: bool,
    // Only emitted values belonging to the current bounded page are retained.
    emitted: BTreeMap<MessageId, Vec<u8>>,
}

#[derive(Serialize)]
struct Updates<'a> {
    r#type: &'static str,
    #[serde(rename = "sessionID", serialize_with = "super::serialize_session")]
    session: &'a SessionId,
    data: Vec<&'a Message>,
}

impl JsonlMessages {
    pub(super) fn emit<O: Write, E: Write>(
        &mut self,
        session: &SessionId,
        page: &SessionMessagesResponseDto,
        terminal: bool,
        output: &mut BoundedOutput<O, E>,
    ) -> Result<(), RunError> {
        let mut next = BTreeMap::new();
        let mut updates = Vec::new();
        for row in page.data().get().as_slice() {
            let id = id(row);
            let previous = self.emitted.remove(id);
            if !self.initialized || terminal || complete(row) {
                let encoded = serde_json::to_vec(row).map_err(|_| ApiClientError::Encode)?;
                if previous.as_ref() != Some(&encoded) {
                    updates.push(row);
                }
                next.insert(id.clone(), encoded);
            } else if let Some(previous) = previous {
                next.insert(id.clone(), previous);
            }
        }
        if !self.initialized {
            output.machine(page)?;
        } else if !updates.is_empty() {
            output.machine(&Updates {
                r#type: "session.messages.upsert",
                session,
                data: updates,
            })?;
        }
        self.initialized = true;
        self.emitted = next;
        Ok(())
    }
}

fn complete(row: &Message) -> bool {
    match row {
        Message::Assistant(row) => row.time().get().completed().as_present().is_some(),
        Message::Shell(row) => row.time().get().completed().as_present().is_some(),
        Message::Compaction(row) => !matches!(row.as_ref(), Compaction::Running(_)),
        _ => true,
    }
}

fn id(row: &Message) -> &MessageId {
    match row {
        Message::AgentSelected(row) => row.id().get().as_inner(),
        Message::ModelSelected(row) => row.id().get().as_inner(),
        Message::User(row) => row.id().get().as_inner(),
        Message::Synthetic(row) => row.id().get().as_inner(),
        Message::System(row) => row.id().get().as_inner(),
        Message::Skill(row) => row.id().get().as_inner(),
        Message::Shell(row) => row.id().get().as_inner(),
        Message::Assistant(row) => row.id().get().as_inner(),
        Message::Compaction(row) => match row.as_ref() {
            Compaction::Running(row) => row.id().get().as_inner(),
            Compaction::Completed(row) => row.id().get().as_inner(),
            Compaction::Failed(row) => row.id().get().as_inner(),
        },
    }
}

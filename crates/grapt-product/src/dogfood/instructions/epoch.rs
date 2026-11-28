//! Model context uses the owner's immutable epoch baseline and ordered deltas.
//! Copyright 2025 OpenAI (Apache-2.0).
//! Modified: typed durable instruction keys and sequences replace text matching.
use super::*;
use grapt_instruction::{InstructionChange, InstructionHistory, InstructionUpdate};
use std::collections::BTreeMap;
fn message(content: String) -> CodexInputItem {
    CodexInputItem::Message(InputMessage {
        role: MessageRole::System,
        content,
    })
}
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn prepare(
    history: InstructionHistory,
    notices: [Option<String>; 2],
) -> Result<PreparedInstructions, String> {
    let initial_session_index = history
        .initial
        .iter()
        .filter(|instruction| {
            !instruction.value.is_empty() && !super::tool_epoch::metadata(instruction.key.as_str())
        })
        .position(|instruction| instruction.key.as_str() == "session/id");
    let initial_frozen = history
        .initial
        .iter()
        .find(|instruction| instruction.key.as_str() == super::tool_epoch::FROZEN)
        .map(|instruction| &instruction.value);
    let initial_catalog = history
        .initial
        .iter()
        .find(|instruction| instruction.key.as_str() == super::tool_epoch::CURRENT)
        .filter(|instruction| Some(&instruction.value) != initial_frozen)
        .map(|instruction| message(super::tool_epoch::update(&instruction.value)));
    let mut initial: Vec<_> = history
        .initial
        .iter()
        .filter(|instruction| {
            !instruction.value.is_empty() && !super::tool_epoch::metadata(instruction.key.as_str())
        })
        .map(|instruction| {
            if instruction.key.as_str() == "session/id" {
                // Preserve the original text; native cache projection uses the typed index.
                message(instruction.value.clone())
            } else {
                message(format!(
                    "Instruction source {}:\n{}",
                    instruction.key.as_str(),
                    instruction.value
                ))
            }
        })
        .collect();
    initial.extend(initial_catalog);
    let mut current: BTreeMap<_, _> = history
        .initial
        .into_iter()
        .map(|instruction| (instruction.key, instruction.value))
        .collect();
    for update in &history.updates {
        for change in &update.changes {
            match change {
                InstructionChange::Value(instruction) => {
                    current.insert(instruction.key.clone(), instruction.value.clone());
                }
                InstructionChange::Removed { key } => {
                    current.remove(key);
                }
            }
        }
    }
    let current_session_index = current
        .iter()
        .filter(|(key, value)| !value.is_empty() && !super::tool_epoch::metadata(key.as_str()))
        .position(|(key, _)| key.as_str() == "session/id");
    let frozen = current
        .iter()
        .find(|(key, _)| key.as_str() == super::tool_epoch::FROZEN)
        .map(|(_, value)| value);
    let frozen_tools = frozen
        .map(|value| super::tool_epoch::decode(value))
        .transpose()?;
    let current_tools = current
        .iter()
        .find(|(key, _)| key.as_str() == super::tool_epoch::CURRENT)
        .map(|(_, value)| value);
    let tool_update = current_tools
        .filter(|value| frozen != Some(*value))
        .map(|value| message(super::tool_epoch::update(value)));
    let mut bytes = tool_update.as_ref().map_or(0, |item| match item {
        CodexInputItem::Message(message) => message.content.len(),
        _ => 0,
    });
    let current = current
        .into_iter()
        .filter(|(key, value)| !value.is_empty() && !super::tool_epoch::metadata(key.as_str()))
        .map(|(_, value)| {
            bytes = bytes
                .checked_add(value.len())
                .ok_or("instruction size overflow")?;
            if bytes > MAX_INSTRUCTION_BYTES {
                return Err("effective instructions exceed 1 MiB".to_owned());
            }
            Ok(message(value))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut current = current;
    current.extend(tool_update);
    // The immutable catalog is encoded in the tools header. Its metadata
    // must not also become a duplicate system message on an upgraded Session.
    let updates = history
        .updates
        .into_iter()
        .filter_map(|mut update| {
            update.changes.retain(|change| match change {
                InstructionChange::Value(value) => value.key.as_str() != super::tool_epoch::FROZEN,
                InstructionChange::Removed { key } => key.as_str() != super::tool_epoch::FROZEN,
            });
            (!update.changes.is_empty()).then_some(update)
        })
        .collect();
    Ok(PreparedInstructions {
        initial_session_index,
        current_session_index,
        subagent_context: None,
        tools: super::tool_epoch::SessionTools::frozen(frozen_tools),
        initial,
        current,
        updates,
        notices: notices.into_iter().flatten().map(message).collect(),
    })
}
pub(in crate::dogfood) fn update(update: &InstructionUpdate) -> CodexInputItem {
    let mut text = String::new();
    for change in &update.changes {
        match change {
            InstructionChange::Value(instruction) => {
                if instruction.key.as_str() == super::tool_epoch::CURRENT {
                    text.push_str(&super::tool_epoch::update(&instruction.value));
                    text.push_str("\n\n");
                    continue;
                }
                text.push_str(&format!(
                    "Instruction source {} replaces its previously supplied value:\n{}\n\n",
                    instruction.key.as_str(),
                    instruction.value
                ));
            }
            InstructionChange::Removed { key } => {
                text.push_str(&format!("Instruction source {} was removed; its previously supplied value no longer applies.\n\n", key.as_str()));
            }
        }
    }
    message(text)
}

impl PreparedInstructions {
    pub(in crate::dogfood) fn prefix(
        &self,
        epochs: bool,
        include_session: bool,
    ) -> Vec<CodexInputItem> {
        let (items, index) = if epochs {
            (&self.initial, self.initial_session_index)
        } else {
            (&self.current, self.current_session_index)
        };
        items
            .iter()
            .enumerate()
            .filter(|(position, _)| include_session || Some(*position) != index)
            .map(|(_, item)| item.clone())
            .collect()
    }

    /// Uncached identity always names the current Session, even when its
    /// shared instruction prefix retains an ancestor's immutable baseline.
    pub(in crate::dogfood) fn session_context(&self) -> Option<&str> {
        if let Some(context) = &self.subagent_context {
            return Some(context);
        }
        match self.current.get(self.current_session_index?)? {
            CodexInputItem::Message(message) => Some(&message.content),
            _ => None,
        }
    }
}

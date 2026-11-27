//! Whole historical user messages before the summary, selected newest first.
//!
//! Copyright 2025 OpenAI, Apache-2.0.
//! A message that cannot fit is omitted whole, never cut.

use super::*;

/// Retention heuristic, not provider-reported usage. One local
/// token is four UTF-8 bytes, rounded up per message. Full request admission
/// independently uses its conservative calibrated bound. This deterministic
/// rule is versioned with the descriptor, not with volatile calibration, so
/// restart, later requests and inherited history select identical bytes.
const USER_TOKENS: usize = 4096;

pub(super) const LEDGER_LABEL: &str = "[Harness-retained settled tool calls from durable history. Exact records, not model-written; omitted tool output is recoverable with the history tool.]\n";

pub(super) fn prefix(
    inbox: &Inbox,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    current: &MessageId,
    until: grapt_types::EventSeq,
    original_users: bool,
) -> Result<Vec<CodexInputItem>, String> {
    if !original_users {
        return Ok(
            retained_facts::retained_facts(inbox, artifacts, session, current, until)?
                .into_iter()
                .map(user)
                .collect(),
        );
    }
    let sequences = grapt_inbox::promoted_read::newest_user_sequences(
        inbox.store(),
        session,
        current,
        until,
        MAX_CONTINUATION_ROWS,
    )?;
    let mut remaining = USER_TOKENS;
    let mut input = Vec::new();
    for sequence in sequences {
        // Decoding and image metadata remain owned by Inbox. Read text only
        // until the whole-message budget is exhausted, not for a lifetime page.
        let message = inbox
            .promoted_message_at(session, sequence)
            .map_err(|error| format!("retained original input is unreadable: {error}"))?
            .ok_or("selected original input disappeared")?;
        let mut tokens = message.text.len().div_ceil(4);
        if tokens > remaining {
            break;
        }
        let item = super::super::prompt_images::input(artifacts, &message)?;
        if let CodexInputItem::ImageUser { images, .. } = &item {
            tokens = tokens.saturating_add(
                usize::try_from(images.estimated_tokens())
                    .map_err(|_| "retained image token estimate exceeds this platform")?,
            );
        }
        if tokens > remaining {
            break;
        }
        remaining -= tokens;
        input.push(item);
    }
    input.reverse();
    // A ledger contains text about completed calls, never replayed calls or
    // results. Exact tail exchanges retain their existing indivisible cut.
    let mut facts = String::new();
    retained_facts::tool_ledger(inbox.store(), artifacts, session, until, &mut facts)?;
    if !facts.is_empty() {
        input.push(user(format!("{LEDGER_LABEL}{facts}")));
    }
    Ok(input)
}

fn user(content: String) -> CodexInputItem {
    CodexInputItem::Message(InputMessage {
        role: MessageRole::User,
        content,
    })
}

/// Request framing only: the stored summary and the next summarizer's anchor
/// remain exactly the model-written text. Legacy descriptors keep their label.
pub(super) fn labelled_summary(summary: &str, original_users: bool) -> String {
    if original_users {
        super::labelled_summary(&format!("[Earlier user messages are historical, not new requests. Later user corrections and the current input take precedence. Use the summary to track completion; do not repeat completed or cancelled work.]\n{summary}"))
    } else {
        super::labelled_summary(summary)
    }
}

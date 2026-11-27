//! Native cutoff behavior for the encoded contract double.
use super::*;

impl ContractDouble {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn rewind_route(
        &mut self,
        request: &WireRequest,
    ) -> Result<WireResponse, TransportError> {
        let error = |status| {
            Ok(WireResponse {
                status,
                body: Vec::new(),
            })
        };
        let Some(session) = Self::session_of(request.path()) else {
            return error(404);
        };
        let Some(entry) = self.sessions.get_mut(&session) else {
            return error(404);
        };
        let Ok(limits) = grapt_api::u3a::U3aLimits::production() else {
            return error(500);
        };
        let bytes = request.body().unwrap_or_default();
        let input = if request.operation() == Operation::SessionRewindPreview {
            grapt_api::u3a::decode_request::<grapt_api::u3a::SessionRewindPreviewRequestDto>(
                bytes, limits,
            )
            .ok()
            .and_then(|dto| serde_json::to_value(dto).ok())
        } else {
            grapt_api::u3a::decode_request::<grapt_api::u3a::SessionRewindRequestDto>(bytes, limits)
                .ok()
                .and_then(|dto| serde_json::to_value(dto).ok())
        };
        let Some(input) = input else {
            return error(400);
        };
        if request.operation() == Operation::SessionRewindPreview {
            let Some(prompts) = input
                .get("prompts")
                .and_then(serde_json::Value::as_u64)
                .filter(|count| (1..=200).contains(count))
            else {
                return error(400);
            };
            let selected = entry
                .messages
                .iter()
                .enumerate()
                .rev()
                .filter_map(|(index, message)| match message {
                    DoubleMessage::User(text, id) => Some((index, text, id)),
                    _ => None,
                })
                .nth(prompts as usize - 1);
            let Some((index, text, id)) = selected else {
                return error(400);
            };
            let before = id
                .clone()
                .unwrap_or_else(|| format!("msg_{:012x}", index + 1));
            return Self::ok(serde_json::json!({"data":{
                "sessionID":session.spelled(), "messageID":before,
                "tip":entry.next_seq, "prompt":text,
            }}));
        }
        let Some(before) = input.get("messageID").and_then(serde_json::Value::as_str) else {
            return error(400);
        };
        let Some(tip) = input.get("tip").and_then(serde_json::Value::as_u64) else {
            return error(400);
        };
        if let Some((_, _, response)) = entry
            .rewind_receipts
            .iter()
            .find(|(held, old_tip, _)| held == before && *old_tip == tip)
        {
            return Self::ok(response.clone());
        }
        if tip != entry.next_seq
            || entry.rewind_receipts.len() >= self.bounds.max_retained_log_entries
        {
            return error(409);
        }
        let selected =
            entry
                .messages
                .iter()
                .enumerate()
                .find_map(|(index, message)| match message {
                    DoubleMessage::User(text, id)
                        if id.as_deref().map_or_else(
                            || format!("msg_{:012x}", index + 1) == before,
                            |id| id == before,
                        ) =>
                    {
                        Some((index, text.clone()))
                    }
                    _ => None,
                });
        let Some((index, prompt)) = selected else {
            return error(400);
        };
        let Some(committed) = tip.checked_add(1) else {
            return error(409);
        };
        let response = serde_json::json!({"data":{
            "sessionID":session.spelled(), "messageID":before,
            "committedSeq":committed, "prompt":prompt,
        }});
        entry.messages.truncate(index);
        entry.next_seq = committed;
        entry
            .rewind_receipts
            .push((before.to_owned(), tip, response.clone()));
        self.pending.clear();
        self.closed = true;
        Self::ok(response)
    }
}

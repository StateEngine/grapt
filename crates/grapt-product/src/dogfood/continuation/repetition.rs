//! A compaction that could only re-summarize what the previous one already
//! summarized is no progress (agency amendment 4.4: failure and no-progress
//! are typed outcomes that leave history recoverable).
//!
//! The historic tight-budget trials show the shape: every summary recorded that
//! `read task.txt` had completed, the model read it again, the identical result
//! forced the next compaction before the model ever saw it, and ten or more
//! cycles ran until the task timed out. Stopping on the first identical cycle
//! keeps every durable fact and returns the decision to the user.

use super::retained_facts::{called, model_terminals};
use super::*;

/// Settled calls compared on each side. More new calls than this since the
/// last compaction is itself evidence of progress.
const MAX_REPEAT_WINDOW: usize = 32;

#[derive(PartialEq, Eq)]
struct CallKey {
    tool: String,
    arguments: String,
    outcome: String,
    content: Option<(u64, grapt_types::ArtifactDigest)>,
}

impl DurableContinuation {
    /// Whether durable history the latest compaction did not summarize exists
    /// besides the current input: an earlier user input or an assistant
    /// message after its source head. Without any, compacting again could only
    /// re-summarize the summary. `true` before the first compaction.
    pub(in crate::dogfood) fn compactable_since_compaction(&self) -> Result<bool, String> {
        self.read_projection(Self::compactable_since_compaction_at)
    }

    fn compactable_since_compaction_at(&self) -> Result<bool, String> {
        let inbox = lock(&self.inbox);
        let Some(retained) =
            retained::RetainedContext::load(&inbox, &self.artifacts, &self.session, &self.input)?
        else {
            return Ok(true);
        };
        let head = retained.source_head.value();
        if grapt_inbox::operation_mail::has_presented_since(
            inbox.store(),
            &self.session,
            retained.source_head,
        )? {
            return Ok(true);
        }
        match inbox.promoted_messages_from(
            &self.session,
            retained.from,
            &retained.generation,
            MAX_CONTINUATION_ROWS,
        ) {
            Ok(users)
                if users
                    .iter()
                    .any(|user| user.seq > head && user.input != self.input) =>
            {
                return Ok(true)
            }
            Ok(_) => {}
            Err(grapt_inbox::InboxError::PromotedPageTooLong { .. }) => return Ok(true),
            Err(error) => return Err(format!("inputs since compaction are unreadable: {error}")),
        }
        let messages = SessionHistoryStore::over(inbox.store())
            .fold_from(&self.session, retained.from)
            .map_err(|error| format!("history since compaction is unreadable: {error}"))?
            .messages()
            .map_err(|error| format!("history since compaction is unreadable: {error}"))?;
        Ok(messages.iter().any(|message| message.opened_at_seq > head))
    }

    /// `Some(reason)` when every model-visible tool call settled since the last
    /// compaction exactly repeats a call, arguments and result that compaction
    /// already summarized, and no new user input arrived meanwhile.
    pub(in crate::dogfood) fn repeated_since_compaction(&self) -> Result<Option<String>, String> {
        self.read_projection(Self::repeated_since_compaction_at)
    }

    fn repeated_since_compaction_at(&self) -> Result<Option<String>, String> {
        let inbox = lock(&self.inbox);
        let Some(retained) =
            retained::RetainedContext::load(&inbox, &self.artifacts, &self.session, &self.input)?
        else {
            return Ok(None);
        };
        let head = retained.source_head;
        if grapt_inbox::operation_mail::has_presented_since(inbox.store(), &self.session, head)? {
            return Ok(None);
        }
        match inbox.promoted_messages_from(
            &self.session,
            retained.from,
            &retained.generation,
            MAX_CONTINUATION_ROWS,
        ) {
            Ok(users)
                if users.iter().any(|user| {
                    user.seq > head.value() && user.kind == grapt_inbox::PromotedInputKind::User
                }) =>
            {
                return Ok(None)
            }
            Ok(_) => {}
            // A longer page than the continuation bound is new input as well.
            Err(grapt_inbox::InboxError::PromotedPageTooLong { .. }) => return Ok(None),
            Err(error) => return Err(format!("inputs since compaction are unreadable: {error}")),
        }
        let store = inbox.store();
        let open = grapt_types::EventSeq::new(i64::MAX.unsigned_abs());
        let since = model_terminals(
            store,
            &self.session,
            Some(head),
            open,
            MAX_REPEAT_WINDOW + 1,
        )?;
        if since.is_empty() || since.len() > MAX_REPEAT_WINDOW {
            return Ok(None);
        }
        let summarized =
            model_terminals(store, &self.session, None, retained.from, MAX_REPEAT_WINDOW)?
                .iter()
                .map(|(_, fact)| self.key(store, fact, retained.from))
                .collect::<Result<Vec<_>, String>>()?;
        let mut repeated = None;
        for (_, fact) in &since {
            let key = self.key(store, fact, open)?;
            if !summarized.contains(&key) {
                return Ok(None);
            }
            if repeated.is_none() {
                repeated = Some(key);
            }
        }
        Ok(repeated.map(|key| {
            format!(
                "the {} tool call(s) since the last compaction repeat results it already summarized ({} {}); compacting again would not make progress",
                since.len(),
                key.tool,
                super::retained_facts::preview(&key.arguments),
            )
        }))
    }

    fn key(
        &self,
        store: &EventStore,
        fact: &session_tool_terminal::ToolTerminalFact,
        until: grapt_types::EventSeq,
    ) -> Result<CallKey, String> {
        let (tool, arguments) = called(store, &self.artifacts, &self.session, fact.call(), until)?
            .ok_or("a settled tool call has no recorded call")?;
        let reference = |output: &Option<grapt_types::ToolTerminalArtifact>| {
            output.as_ref().map(|output| {
                (
                    output.reference().length(),
                    output.reference().digest().clone(),
                )
            })
        };
        let (outcome, content) = match fact.outcome() {
            ToolTerminalOutcome::Completed { output } => {
                ("completed".to_owned(), reference(output))
            }
            ToolTerminalOutcome::Failed { reason, detail } => {
                (format!("failed {reason:?}"), reference(detail))
            }
            ToolTerminalOutcome::Cancelled { by } => (format!("cancelled {by:?}"), None),
            ToolTerminalOutcome::ResultMissing => ("missing".to_owned(), None),
        };
        Ok(CallKey {
            tool,
            arguments,
            outcome,
            content,
        })
    }
}

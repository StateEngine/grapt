//! Session-scoped durable reads and tool formatting for context history pages.
use super::*;
use grapt_context::history::{CallRow, ListMetadata, Outcome, Source};
use grapt_types::EventSeq;

pub(in crate::dogfood) fn history(
    inbox: &Arc<Mutex<Inbox>>,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    call: Option<&ToolCallId>,
    cursor: Option<u64>,
    offset: Option<u64>,
) -> Result<Vec<u8>, String> {
    let source = SessionHistory {
        inbox,
        artifacts,
        session,
    };
    grapt_context::history::page(&source, call, cursor, offset)
}

struct SessionHistory<'a> {
    inbox: &'a Arc<Mutex<Inbox>>,
    artifacts: &'a FilesystemArtifactStore,
    session: &'a SessionId,
}

impl Source for SessionHistory<'_> {
    fn output(&self, call: &ToolCallId) -> Result<String, String> {
        let Self {
            inbox,
            artifacts,
            session,
        } = *self;
        // An id this session never recorded (a model's guess, such as a
        // copied example) is told how to find a real one.
        let recorded = {
            let inbox = lock(inbox);
            let window = BoundedWindow {
                from: None,
                until: None,
                max_rows: MAX_CONTINUATION_ROWS,
            };
            stored_calls_for(inbox.store(), session, artifacts, window, Some(call), None)?
                .iter()
                .any(|entry| entry.call == *call)
        };
        if recorded {
            let projection = tool_projection(inbox, artifacts, session, call)?;
            if projection.settled {
                return Ok(projection.output);
            }
            // Listed as failed, so fetched as the same notice.
            if names_unreadable_terminal(&lock(inbox), session, call)? {
                return Ok(UNREADABLE_OUTCOME.to_owned());
            }
            Err(format!("tool {call} has no settled outcome"))
        } else {
            super::super::background::nested_output(inbox, artifacts, session, call)?.ok_or_else(
                || {
                    format!(
                        "no settled call {call} in this session; list calls with {{}} to find one"
                    )
                },
            )
        }
    }

    fn calls_after(&self, cursor: EventSeq, limit: usize) -> Result<Vec<CallRow>, String> {
        let cursor = i64::try_from(cursor.value())
            .map_err(|_| "history cursor exceeds SQLite range".to_owned())?;
        let inbox = lock(self.inbox);
        let limit = i64::try_from(limit).map_err(|_| "history list bound invalid")?;
        let visible = grapt_store::history_visibility::HistoryVisibility::read(
            inbox.store().connection(),
            self.session,
        )
        .map_err(|error| error.to_string())?
        .sql("e.seq");
        let mut statement = inbox
            .store()
            .connection()
            .prepare(&format!(
                "SELECT e.seq, e.data FROM event e
                 WHERE e.aggregate_family = 'session' AND e.aggregate_id = ?1 AND {visible}
                   AND e.seq > ?2 AND e.type = 'session.content.appended.1'
                   AND CASE
                         WHEN json_valid(e.data) = 0 THEN 1
                         WHEN json_type(e.data, '$.item.kind') IS NOT 'text' THEN 1
                         WHEN json_extract(e.data, '$.item.kind') = 'tool' THEN 1
                         ELSE 0
                       END = 1
                 ORDER BY e.seq LIMIT ?3",
            ))
            .map_err(|error| format!("history list query failed: {error}"))?;
        let mapped = statement
            .query_map((self.session.as_str(), cursor, limit), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("history list query failed: {error}"))?;
        let mut rows = Vec::new();
        for row in mapped {
            let (seq, data) = row.map_err(|error| format!("history list row failed: {error}"))?;
            let seq = u64::try_from(seq).map_err(|_| "history call has negative sequence")?;
            rows.push(CallRow {
                seq: EventSeq::new(seq),
                data,
            });
        }
        Ok(rows)
    }

    /// Project metadata without fetching arguments, output, progress or final-file
    /// artifacts. Each existing owner caps its rows independently of session lifetime.
    fn metadata(
        &self,
        seq: EventSeq,
        message: &MessageId,
        target: &ToolCallId,
    ) -> Result<Option<ListMetadata>, String> {
        let inbox = lock(self.inbox);
        let session = self.session;
        let call_seq = seq.value();
        let rows = scoped_terminals(
            inbox.store(),
            session,
            None,
            None,
            Scope::Model(Some(target)),
            MAX_CONTINUATION_ROWS,
        )?;
        let mut terminal = None;
        let mut unreadable = false;
        for row in rows {
            let fact = match row.terminal {
                StoredTerminal::Readable(fact) => fact,
                // A row naming this call that cannot be read keeps the call
                // listed, as failed; it proves no other outcome.
                StoredTerminal::Unreadable { .. } => {
                    unreadable |= row.names_target;
                    continue;
                }
            };
            if fact.call() != target {
                return Err("history list selected a terminal for another tool".to_owned());
            }
            if terminal.is_some() {
                return Err(format!(
                    "tool {target} has duplicate durable terminal outputs"
                ));
            }
            terminal = Some(list_terminal(fact.outcome())?);
        }
        let unreadable = unreadable.then(|| (Outcome::Failed, UNREADABLE_OUTCOME.len() as u64));
        let Some((outcome, output_bytes)) = terminal.or(unreadable) else {
            return Ok(None);
        };

        let identities = tool_identities(
            inbox.store(),
            session,
            None,
            None,
            Some(target),
            MAX_CONTINUATION_ROWS,
        )?;
        let permissions = permission_calls_for(
            inbox.store(),
            session,
            None,
            None,
            Some(target),
            MAX_CONTINUATION_ROWS,
        )?;
        let identity = identities.iter().find(|held| held.call == *target);
        if identity.is_some_and(|held| held.message != *message || held.seq >= call_seq) {
            return Err("tool identity does not precede its own assistant call".to_owned());
        }
        let permission = permissions.iter().find(|held| held.call == *target);
        if identity
            .zip(permission)
            .is_some_and(|(identity, permission)| {
                !tool_matches_permission(&identity.tool, &permission.tool)
            })
        {
            return Err("tool identity conflicts with its permission action".to_owned());
        }
        let name = identity
            .map(|held| held.tool.as_str())
            .or_else(|| permission.map(|held| held.tool.as_str()))
            .unwrap_or("unavailable tool")
            .to_owned();
        Ok(Some(ListMetadata {
            name,
            outcome,
            output_bytes,
        }))
    }
}

/// List metadata reports durable result bytes without opening the artifact.
/// A call-targeted history page remains the exact model-visible projection and
/// reports its own exact `totalBytes`, including legacy UTF-8 replacement text.
/// What a listed call whose only terminal row is unreadable reports.
const UNREADABLE_OUTCOME: &str = "The recorded tool outcome is unreadable";

/// Whether a terminal row that names `target` cannot be read.
fn names_unreadable_terminal(
    inbox: &Inbox,
    session: &SessionId,
    target: &ToolCallId,
) -> Result<bool, String> {
    let rows = scoped_terminals(
        inbox.store(),
        session,
        None,
        None,
        Scope::Model(Some(target)),
        MAX_CONTINUATION_ROWS,
    )?;
    Ok(rows
        .iter()
        .any(|row| row.names_target && matches!(row.terminal, StoredTerminal::Unreadable { .. })))
}

fn list_terminal(outcome: &ToolTerminalOutcome) -> Result<(Outcome, u64), String> {
    let rendered = |prefix: String, artifact: Option<&grapt_types::ToolTerminalArtifact>| {
        let prefix = u64::try_from(prefix.len())
            .map_err(|_| "history terminal prefix exceeds u64".to_owned())?;
        match artifact {
            Some(artifact) if artifact.reference().length() > 0 => prefix
                .checked_add(1)
                .and_then(|bytes| bytes.checked_add(artifact.reference().length()))
                .ok_or_else(|| "history terminal byte count overflowed".to_owned()),
            _ => Ok(prefix),
        }
    };
    match outcome {
        ToolTerminalOutcome::Completed { output } => Ok((
            Outcome::Completed,
            output
                .as_ref()
                .map_or(0, |value| value.reference().length()),
        )),
        ToolTerminalOutcome::Failed { reason, detail } => Ok((
            Outcome::Failed,
            rendered(failure_line(*reason), detail.as_ref())?,
        )),
        ToolTerminalOutcome::Cancelled { by } => Ok((
            Outcome::Failed,
            u64::try_from(format!("Tool cancelled: {by:?}").len())
                .map_err(|_| "history cancellation text exceeds u64".to_owned())?,
        )),
        ToolTerminalOutcome::ResultMissing => Ok((
            Outcome::Failed,
            u64::try_from("The tool result is missing".len())
                .map_err(|_| "history missing-result text exceeds u64".to_owned())?,
        )),
    }
}

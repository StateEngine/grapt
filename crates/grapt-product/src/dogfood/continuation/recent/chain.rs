//! Replay compaction lineage in bounded batches, retaining only selected public rows.
use super::*;

/// Existing admission estimates, distinct from provider call usage.
#[derive(Debug, Clone)]
pub(in crate::dogfood) struct TokenEstimates {
    pub before: u64,
    pub after: Option<u64>,
    pub basis: &'static str,
}

/// A validated compaction state in durable order.
#[derive(Debug)]
pub(in crate::dogfood) struct ChainStep {
    pub seq: u64,
    /// The committed `session.compaction.ended.2` event identity.
    pub event: String,
    /// The selection's retained cut: the window start for the next step.
    pub from: u64,
    /// The public recent value this step published.
    pub recent: Utf16Text,
    pub token_estimates: Option<TokenEstimates>,
}

/// The runtime needs the latest state, never an accumulated lifetime vector.
pub(in crate::dogfood) fn recent_chain(
    inbox: &Inbox,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
) -> Result<Vec<ChainStep>, String> {
    let mut latest = Vec::with_capacity(1);
    ChainCursor::new().visit(
        (inbox, artifacts, session),
        i64::MAX,
        MAX_CONTINUATION_ROWS,
        |step| {
            latest.clear();
            latest.push(step);
        },
    )?;
    Ok(latest)
}

/// Read-local replay state: advancing a page never reconstructs its prefix.
pub(in crate::dogfood) struct ChainCursor {
    after: i64,
    window_from: u64,
    previous_recent: Utf16Text,
}

impl ChainCursor {
    pub(in crate::dogfood) fn new() -> Self {
        Self {
            after: -1,
            window_from: 0,
            previous_recent: Utf16Text::empty(),
        }
    }

    pub(in crate::dogfood) fn selected_through(
        &mut self,
        inbox: &Inbox,
        artifacts: &FilesystemArtifactStore,
        session: &SessionId,
        selected: &[u64],
        through: u64,
    ) -> Result<Vec<ChainStep>, String> {
        let through = i64::try_from(through).map_err(|error| error.to_string())?;
        let mut steps = Vec::with_capacity(selected.len());
        self.visit(
            (inbox, artifacts, session),
            through,
            selected.len().max(1),
            |step| {
                if selected.contains(&step.seq) {
                    steps.push(step);
                }
            },
        )?;
        Ok(steps)
    }

    fn visit(
        &mut self,
        (inbox, artifacts, session): (&Inbox, &FilesystemArtifactStore, &SessionId),
        through: i64,
        batch: usize,
        mut take: impl FnMut(ChainStep),
    ) -> Result<(), String> {
        if through <= self.after {
            return Ok(());
        }
        let visible = grapt_store::history_visibility::HistoryVisibility::read(
            inbox.store().connection(),
            session,
        )
        .map_err(|error| error.to_string())?
        .sql("seq");
        let sql_batch = i64::try_from(batch).map_err(|error| error.to_string())?;
        loop {
            let mut statement = inbox.store().connection().prepare(&format!(
            "SELECT CASE WHEN length(CAST(id AS BLOB)) <= {MAX_RECENT_ID_BYTES} THEN id END, seq,
             CASE WHEN length(CAST(data AS BLOB)) <= {MAX_DESCRIPTOR_BYTES} THEN data END
             FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND {visible}
             AND type='session.compaction.ended.2' AND (seq>?2 OR seq<0) AND seq<=?3 ORDER BY seq LIMIT ?4"))
            .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map((session.as_str(), self.after, through, sql_batch), |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            let count = rows.len();
            for (event, seq, data) in rows {
                let event = event.ok_or_else(|| format!("recent compaction event identity exceeds its {MAX_RECENT_ID_BYTES}-byte read bound"))?;
                let step = chain_step(
                    (inbox, artifacts, session),
                    &event,
                    (seq, data),
                    self.window_from,
                    &self.previous_recent,
                )
                .map_err(|error| format!("compaction {event}: {error}"))?;
                self.window_from = step.from;
                self.previous_recent = step.recent.clone();
                self.after = seq;
                take(step);
            }
            if count < batch {
                return Ok(());
            }
        }
    }
}

/// One committed compaction's lineage step, after the steps before it.
fn chain_step(
    (inbox, artifacts, session): (&Inbox, &FilesystemArtifactStore, &SessionId),
    event: &str,
    (seq, data): (i64, Option<String>),
    window_from: u64,
    previous_recent: &Utf16Text,
) -> Result<ChainStep, String> {
    #[cfg(test)]
    CHAIN_STEPS.with(|steps| steps.set(steps.get() + 1));
    let seq =
        u64::try_from(seq).map_err(|_| "recent lineage has a negative sequence".to_owned())?;
    let data = data.ok_or("compaction descriptor exceeds its public bound".to_owned())?;
    let value: serde_json::Value = serde_json::from_str(&data)
        .map_err(|error| format!("compaction descriptor is not JSON: {error}"))?;
    let descriptor = grapt_event::session_compaction::decode(&value)
        .map_err(|error| format!("compaction descriptor is unreadable: {error}"))?;
    if descriptor.source_session() != session {
        return Err("compaction descriptor names another Session".to_owned());
    }
    let RetainedHistory::DeterministicSelection {
        descriptor: selection_ref,
    } = descriptor.retained()
    else {
        return Err("committed context has no deterministic selection".to_owned());
    };
    if descriptor.baseline() != selection_ref
        || descriptor.next_request().context() != selection_ref
    {
        return Err("retained baseline and request context disagree with selection".to_owned());
    }
    if selection_ref.length() > MAX_SELECTION_BYTES {
        return Err("retained selection exceeds its artifact bound".to_owned());
    }
    let raw = read_text(
        artifacts,
        selection_ref,
        CaptureDisposition::Complete,
        "retained selection",
    )
    .map_err(|error| format!("recent retained selection is unreadable: {error}"))?;
    let selection = Selection::decode(raw.as_str())
        .map_err(|error| format!("retained selection is malformed: {error}"))?;
    if &selection.session != session
        || &selection.compaction != descriptor.id()
        || selection.from > selection.source_head
        || selection.source_head >= seq
    {
        return Err("retained selection does not bind the committed lineage".to_owned());
    }
    if selection.from < window_from {
        return Err("recent lineage regressed across a committed compaction".to_owned());
    }
    let public = window_recent(
        inbox,
        artifacts,
        session,
        window_from,
        selection.source_head,
        previous_recent,
    )?;
    Ok(ChainStep {
        seq,
        event: event.to_owned(),
        from: selection.from,
        recent: public,
        token_estimates: selection
            .admission
            .as_ref()
            // Structural limits may count rows or bytes rather than tokens.
            .filter(|admission| {
                matches!(admission.estimate_basis, "confirmed-prefix" | "estimated")
            })
            .map(|admission| TokenEstimates {
                before: admission.estimate,
                after: admission.rebuilt_estimate,
                basis: admission.estimate_basis,
            }),
    })
}

//! Shared deterministic durable-owner witness fixtures.
use super::*;

pub(super) fn session() -> SessionId {
    match "ses_df0".parse() {
        Ok(session) => session,
        Err(error) => panic!("ses_df0 is a valid session identity: {error}"),
    }
}

pub(super) fn attempt(raw: &str) -> PhysicalAttemptId {
    match PhysicalAttemptId::new(raw) {
        Some(attempt) => attempt,
        None => panic!("{raw} is a non-empty attempt identity"),
    }
}

pub(super) fn step(value: u32) -> StepNumber {
    match StepNumber::new(value) {
        Some(step) => step,
        None => panic!("{value} is a positive step number"),
    }
}

pub(super) fn store() -> Arc<Mutex<EventStore>> {
    match EventStore::open_in_memory() {
        Ok(store) => Arc::new(Mutex::new(store)),
        Err(error) => panic!("an in-memory store must open: {error}"),
    }
}

pub(super) fn state() -> AdapterState {
    AdapterState::none()
}

pub(super) fn text_delta(block: u32, text: &str) -> DurableFact {
    DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::TextDelta {
            block: BlockOrdinal::new(block),
            text: text.to_owned(),
            state: state(),
        }),
    }
}

pub(super) fn text_opened(block: u32) -> DurableFact {
    DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::TextOpened {
            block: BlockOrdinal::new(block),
            state: state(),
        }),
    }
}

pub(super) fn text_closed(block: u32) -> DurableFact {
    DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::TextClosed {
            block: BlockOrdinal::new(block),
            state: state(),
        }),
    }
}

pub(super) fn reasoning_opened(block: u32) -> DurableFact {
    DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::ReasoningOpened {
            block: BlockOrdinal::new(block),
            state: state(),
        }),
    }
}

pub(super) fn reasoning_delta(block: u32, text: &str) -> DurableFact {
    DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::ReasoningDelta {
            block: BlockOrdinal::new(block),
            text: text.to_owned(),
            state: state(),
        }),
    }
}

pub(super) fn reasoning_closed(block: u32) -> DurableFact {
    DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::ReasoningClosed {
            block: BlockOrdinal::new(block),
            state: state(),
        }),
    }
}

pub(super) fn usage(input: Option<u64>, output: Option<u64>) -> TokenUsage {
    let reported = |value| match TokenCount::new(value) {
        Ok(count) => UsageField::reported(count),
        Err(error) => panic!("fixture token count is bounded: {error}"),
    };
    TokenUsage::new(
        input.map_or(UsageField::ABSENT, reported),
        input.map_or(UsageField::ABSENT, reported),
        output.map_or(UsageField::ABSENT, reported),
        UsageField::ABSENT,
        UsageField::ABSENT,
        UsageField::ABSENT,
        UsageField::ABSENT,
    )
}

pub(super) fn complete_usage() -> TokenUsage {
    let reported = |value| match TokenCount::new(value) {
        Ok(count) => UsageField::reported(count),
        Err(error) => panic!("fixture token count is bounded: {error}"),
    };
    TokenUsage::new(
        reported(14),
        reported(11),
        reported(7),
        reported(3),
        reported(2),
        reported(1),
        reported(21),
    )
}

/// Reads back every durable row on the Session aggregate, oldest first.
pub(super) fn rows(store: &Arc<Mutex<EventStore>>) -> Vec<(u64, String, serde_json::Value)> {
    let guard = lock_event(store);
    let conn = guard.connection();
    let mut statement = match conn.prepare(
        "SELECT seq, type, data FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1 ORDER BY seq",
    ) {
        Ok(statement) => statement,
        Err(error) => panic!("the read-back query must prepare: {error}"),
    };
    let mapped = statement.query_map([session().as_str()], |row| {
        Ok((
            row.get::<_, i64>(0)? as u64,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    });
    let mapped = match mapped {
        Ok(mapped) => mapped,
        Err(error) => panic!("the read-back query must run: {error}"),
    };
    let mut out = Vec::new();
    for row in mapped {
        let (seq, kind, data) = match row {
            Ok(row) => row,
            Err(error) => panic!("a durable row must decode: {error}"),
        };
        let value = match serde_json::from_str(&data) {
            Ok(value) => value,
            Err(error) => panic!("durable data must be JSON: {error}"),
        };
        out.push((seq, kind, value));
    }
    out
}

/// Folds the durable log exactly as a reader would.
pub(super) fn history(store: &Arc<Mutex<EventStore>>) -> MessageHistory {
    let rows = rows(store);
    let mut history = MessageHistory::default();
    for (seq, kind, data) in &rows {
        let (name, version) = match kind.rsplit_once('.') {
            Some((name, version)) => (name, version),
            None => panic!("a stored type is `name.version`"),
        };
        let version: u32 = match version.parse() {
            Ok(version) => version,
            Err(error) => panic!("a stored version is an integer: {error}"),
        };
        if let Err(error) = history.apply(HistoryRow {
            seq: *seq,
            kind_name: name,
            kind_version: version,
            data,
        }) {
            panic!("the owner's own log must fold: {error}");
        }
    }
    history
}

pub(super) fn drive<S: CommittedFactSink>(
    owner: &mut DurableAttemptOwner<'_, FilesystemArtifactStore, S>,
    facts: Vec<DurableFact>,
) {
    for fact in facts {
        if let Err(error) = owner.commit(fact) {
            panic!("the owner must accept this fact: {error}");
        }
    }
}

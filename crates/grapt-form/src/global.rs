//! Typed global ownership on the existing durable form service.
use grapt_store::{
    AggregateId, DecodedFormFact, EventStore, GovernedTransaction, HeldWriters, ProjectorError,
};
use grapt_types::{EventId, FormId, FormOwner, LocationRef, McpElicitationId, ToolCallId};
use serde_json::{json, Value};

use crate::service::{FormError, FormProjector, FormService};

fn bound_payload(value: &Value) -> Result<(), FormError> {
    struct Size(usize);
    impl std::io::Write for Size {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            // Leave room for the typed owner and event coordinates.
            if bytes.len() > (240 * 1024usize).saturating_sub(self.0) {
                return Err(std::io::Error::other("global form payload byte bound"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Size(0), value)
        .map_err(|_| FormError::Global("global form payload byte bound".into()))
}

/// The dispatch that raised an elicitation: the admitted tool call, and the
/// connection generation it was admitted under.
///
/// THE UNIQUENESS CONTRACT, MEASURED RATHER THAN ASSUMED. A provider-supplied
/// call id carries no uniqueness guarantee of its own. The product enforces
/// exactly this much and no more (`grapt-mcp/src/session.rs`):
///
/// - `begin_call` refuses an id that is in flight (`:315`) or remembered as
///   settled (`:318`) — **within one `McpSession`, which is one connection**.
/// - that settled ledger is bounded and EVICTS its oldest entry (`:376-381`),
///   so an id can recur on the same connection once it ages out.
/// - `start` mints a fresh generation and retires the catalog and every
///   in-flight call (`:233-258`), so attempts are distinguishable — but the
///   settled ledger survives a restart, which is why the generation is a
///   coordinate and not a substitute for the id.
///
/// Nothing anywhere refuses a repeated id across two connections, two
/// processes, or two sessions. So the identity of a dispatch is the TRIPLE
/// (`connection`, `generation`, `call`), and `connection` already carries the
/// process generation. A reader correlating a form to a session's
/// `session.tool.*` record must match all three; matching `call` alone would
/// bind a form to whichever session happened to reuse the id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallDispatch {
    pub call: ToolCallId,
    pub generation: u64,
    /// The attempt that admitted the call, when one framed it.
    ///
    /// THIS IS WHAT MAKES THE JOIN EXACT. `call` alone identifies a dispatch
    /// only within one connection's live-plus-remembered set, and that ledger
    /// evicts — so one session can legitimately admit an id twice and a reader
    /// could name the session but not the turn. The attempt is the per-dispatch
    /// identity the SESSION side already records on `session.tool.terminal.1`,
    /// so carrying it here gives both sides a shared coordinate and the pair
    /// (`call`, `attempt`) selects one dispatch.
    ///
    /// `None` where no attempt framed the call, never a minted substitute: a
    /// fabricated attempt would join to nothing and look like a real one.
    pub attempt: Option<String>,
}

impl CallDispatch {
    pub fn wire(&self) -> Value {
        let mut value = json!({"call": self.call.as_str(), "generation": self.generation});
        // ABSENT, NOT NULL, when no attempt framed the call — so a dispatch
        // written without one stays byte-identical to what a pre-attempt build
        // wrote for the same facts.
        if let Some(attempt) = &self.attempt {
            value["attempt"] = json!(attempt);
        }
        value
    }

    pub fn decode(value: &Value) -> Result<Self, String> {
        let call = value
            .get("call")
            .and_then(Value::as_str)
            .filter(|call| call.len() <= 512)
            .ok_or("global form dispatch call is not a bounded string")?;
        let generation = value
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or("global form dispatch generation is not an unsigned integer")?;
        // ABSENT is legal (no attempt framed the call, or a pre-attempt event);
        // present-but-unreadable is corruption and refuses.
        let attempt = match value.get("attempt") {
            None => None,
            Some(Value::String(attempt)) if !attempt.is_empty() && attempt.len() <= 512 => {
                Some(attempt.clone())
            }
            Some(_) => return Err("global form dispatch attempt is not a bounded string".into()),
        };
        Ok(Self {
            call: ToolCallId::new(call).ok_or("global form dispatch call identity")?,
            generation,
            attempt,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalFormBinding {
    pub elicitation: McpElicitationId,
    pub location: LocationRef,
    /// Token scoped to one admitted call on a held MCP connection.
    ///
    /// Minted per call; values from separate calls do not identify connection
    /// continuity.
    pub connection: McpElicitationId,
    /// OBSERVATIONAL, NOT AN OWNERSHIP COORDINATE. The identity of the admitted
    /// tool call whose dispatch raised this elicitation, carried so a reader can
    /// correlate the form with the session's own `session.tool.*` record. It
    /// does not own the form and it never decides who may settle it — the three
    /// fields above do. See `compatible_with`.
    ///
    /// SCOPED BY `connection`, AND THAT IS NOT DEFENSIVE PHRASING — see
    /// `CallDispatch` for the measured uniqueness contract. A call id alone can
    /// recur across connections, processes and sessions, so the identifying
    /// triple is (`connection`, `generation`, `call`).
    pub call: Option<CallDispatch>,
}

impl GlobalFormBinding {
    pub fn owner(&self) -> FormOwner {
        FormOwner::Global(self.elicitation.clone())
    }

    /// Ownership invariants must MATCH; observational metadata must not CONFLICT.
    ///
    /// The guards that use this exist to refuse a form whose connection or
    /// location changed under it. Comparing whole structs with `==` would fold a
    /// new observational field into that judgement, so a form created before
    /// `call` existed and settled after it would compare `None` against `Some`
    /// and be refused — an unsettleable form, from a field that owns nothing.
    ///
    /// Weakening the comparison wholesale would be the opposite error: two
    /// DIFFERENT present call ids mean two different dispatches claiming one
    /// form, which is a genuine conflict and must still refuse. So absence is
    /// compatible in either direction, and disagreement between two present
    /// identities is not.
    pub fn compatible_with(&self, other: &Self) -> bool {
        if self.elicitation != other.elicitation
            || self.connection != other.connection
            || self.location != other.location
        {
            return false;
        }
        match (&self.call, &other.call) {
            (Some(held), Some(offered)) => held == offered,
            _ => true,
        }
    }

    pub fn wire(&self) -> Value {
        let mut value = json!({"elicitation":self.elicitation.as_str(),
            "directory":self.location.directory(),
            "connection":self.connection.as_str()});
        // ABSENT, NOT NULL, when there is no call. The crate's decode policy
        // treats explicit null as a decode error distinct from absence, and a
        // binding written without a call must be byte-identical to one written
        // before this field existed — otherwise every pre-existing global form
        // event would differ from what today's code writes for the same facts.
        if let Some(dispatch) = &self.call {
            value["dispatch"] = dispatch.wire();
        }
        value
    }

    pub fn decode(value: &Value) -> Result<Self, String> {
        let text = |key| {
            value
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("global form binding lacks {key}"))
        };
        let elicitation = text("elicitation")?;
        let directory = text("directory")?;
        let connection = text("connection")?;
        if elicitation.len() > 512 || connection.len() > 512 || directory.len() > 4096 {
            return Err("global form binding exceeds its bound".into());
        }
        // ABSENT is legacy and legal. PRESENT-BUT-UNREADABLE is corruption and is
        // refused: a binding carrying a `dispatch` key it cannot read must not
        // silently become one that never had a dispatch.
        let call = match value.get("dispatch") {
            None => None,
            Some(dispatch) => Some(CallDispatch::decode(dispatch)?),
        };
        Ok(Self {
            elicitation: McpElicitationId::new(elicitation).ok_or("elicitation identity")?,
            location: LocationRef::new(directory, None).map_err(|error| error.to_string())?,
            connection: McpElicitationId::new(connection).ok_or("connection identity")?,
            call,
        })
    }
}

/// Project one global form fact. A SQLite failure keeps its type, so the
/// database failing is never read as this fact being refused.
pub(crate) fn project(
    tx: &rusqlite::Transaction<'_>,
    event: &DecodedFormFact,
) -> Result<(), ProjectorError> {
    let binding = GlobalFormBinding::decode(&event.data["owner"])?;
    if event.kind.name() == "form.created" {
        let form = &event.data["form"];
        let title = form["title"].as_str().ok_or("global form title absent")?;
        if form["id"].as_str() != Some(event.form.as_str()) || form["sessionID"] != "global" {
            return Err("global form projection identity mismatch".into());
        }
        if let Some(error) = crate::answer::validate_fields(&form["fields"]) {
            return Err(error.into());
        }
        tx.execute(
            "INSERT INTO form_request (id, session_id, title, status, global_binding) VALUES (?1, NULL, ?2, 'pending', ?3)",
            rusqlite::params![event.form.as_str(), title, binding.wire().to_string()],
        )?;
        return Ok(());
    }
    let stored: String = tx.query_row(
        "SELECT global_binding FROM form_request WHERE id=?1 AND session_id IS NULL",
        [event.form.as_str()],
        |row| row.get(0),
    )?;
    let stored: Value = serde_json::from_str(&stored).map_err(|error| error.to_string())?;
    // COMPATIBILITY, NOT EQUALITY. The ownership coordinates must match; the
    // observational call identity must not contradict. See `compatible_with`.
    let stored = GlobalFormBinding::decode(&stored)?;
    if !stored.compatible_with(&binding) {
        return Err(if stored.call.is_some() && binding.call.is_some() {
            "global form dispatch identity changed".into()
        } else {
            "global form connection or location changed".into()
        });
    }
    let changed = match event.kind.name() {
        "form.replied" => {
            let answer = event.data.get("answer").filter(|value| value.is_object()).ok_or("global form answer absent")?;
            let raw: String = tx.query_row(
                "SELECT CASE WHEN length(CAST(data AS BLOB)) <= 262144 THEN data END FROM event
                 WHERE aggregate_family='form' AND aggregate_id=?1 AND type='form.created.2' ORDER BY seq LIMIT 1",
                [event.form.as_str()], |row| row.get(0))?;
            let created: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
            if let Some(error) = crate::answer::validate_answer(&created["form"]["fields"], answer) { return Err(error.into()); }
            tx.execute("UPDATE form_request SET status='answered', answer=?2 WHERE id=?1 AND status IN ('pending','suspended')",
                rusqlite::params![event.form.as_str(), answer.to_string()])
        }
        "form.cancelled" => tx.execute("UPDATE form_request SET status='cancelled' WHERE id=?1 AND status IN ('pending','suspended')", [event.form.as_str()]),
        "form.suspended" => {
            let reason = event.data["reason"].as_str().filter(|reason| matches!(*reason, "shutdown" | "process-loss")).ok_or("global form suspension reason absent")?;
            tx.execute("UPDATE form_request SET status='suspended', suspended_reason=?2 WHERE id=?1 AND status='pending'", rusqlite::params![event.form.as_str(), reason])
        }
        _ => return Err("non-global form fact".into()),
    }?;
    if changed != 1 {
        return Err("global form transition lost its live row".into());
    }
    Ok(())
}

impl FormService {
    pub fn catch_up_global_on(
        store: &mut EventStore,
        id: &FormId,
    ) -> Result<grapt_store::CaughtUp, FormError> {
        store
            .catch_up(&AggregateId::Form(id.clone()), &mut FormProjector)
            .map_err(FormError::Store)
    }
    pub fn answer_global_on(
        store: &mut EventStore,
        event: &EventId,
        id: &FormId,
        binding: &GlobalFormBinding,
        answer: &Value,
        created_ms: i64,
    ) -> Result<grapt_types::CasOutcome, FormError> {
        bound_payload(answer)?;
        let outcome = Self::settle_on(
            store,
            event,
            id,
            "form.replied",
            |_| json!({"answer":answer}),
            created_ms,
            Some(binding),
        )?;
        if matches!(
            outcome,
            grapt_types::CasOutcome::Applied | grapt_types::CasOutcome::AlreadyTerminal
        ) {
            store.release_held_writer(&AggregateId::Form(id.clone()));
        }
        Ok(outcome)
    }

    pub fn cancel_global_on(
        store: &mut EventStore,
        event: &EventId,
        id: &FormId,
        binding: &GlobalFormBinding,
        created_ms: i64,
    ) -> Result<grapt_types::CasOutcome, FormError> {
        let outcome = Self::settle_on(
            store,
            event,
            id,
            "form.cancelled",
            |_| json!({}),
            created_ms,
            Some(binding),
        )?;
        if matches!(
            outcome,
            grapt_types::CasOutcome::Applied | grapt_types::CasOutcome::AlreadyTerminal
        ) {
            store.release_held_writer(&AggregateId::Form(id.clone()));
        }
        Ok(outcome)
    }

    /// Check before electing a new global aggregate. Creation checks again in its transaction.
    pub fn check_global_capacity(store: &EventStore) -> Result<(), FormError> {
        check_capacity(store.connection())
    }

    /// The caller elects the form aggregate before asking. Append never elects.
    pub fn create_global_on(
        store: &mut EventStore,
        event: &EventId,
        id: &FormId,
        binding: &GlobalFormBinding,
        info: &Value,
        created_ms: i64,
    ) -> Result<(), FormError> {
        bound_payload(info)?;
        let validated = GlobalFormBinding::decode(&binding.wire()).map_err(FormError::Global)?;
        if &validated != binding {
            return Err(FormError::Global(
                "global form binding changed during validation".into(),
            ));
        }
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        check_capacity(&tx)?;
        let data = json!({"formID":id.as_str(), "owner":binding.wire(), "form":info});
        Self::append_global(&tx, &held, id, event, "form.created", &data, created_ms)?;
        tx.commit()?.discard();
        Ok(())
    }

    pub(crate) fn append_global(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        form: &FormId,
        event: &EventId,
        kind: &str,
        data: &Value,
        created_ms: i64,
    ) -> Result<(), FormError> {
        let aggregate = AggregateId::Form(form.clone());
        EventStore::append_within(
            tx,
            grapt_store::AppendInput {
                id: event,
                writer: held.token(&aggregate)?,
                aggregate,
                kind_name: kind,
                version: grapt_types::EventVersion::new(2).ok_or(FormError::Version)?,
                created_ms,
                data,
            },
            &mut [&mut FormProjector],
        )?;
        Ok(())
    }
}

fn check_capacity(connection: &rusqlite::Connection) -> Result<(), FormError> {
    let pending: i64 = connection.query_row(
        "SELECT COUNT(*) FROM (SELECT 1 FROM form_request WHERE status IN ('pending','suspended') LIMIT 128)",
        [], |row| row.get(0),
    )?;
    if pending >= 128 {
        return Err(FormError::Global("pending form bound reached".into()));
    }
    Ok(())
}

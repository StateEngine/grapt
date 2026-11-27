//! Bounded global form reads, verified against the creation fact.
use crate::{
    catalog::{FormState, SkippedForm},
    global::GlobalFormBinding,
    FormError,
};
use grapt_store::EventStore;
use grapt_types::{FormId, FormSuspensionReason};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct GlobalFormRecord {
    pub id: FormId,
    pub binding: GlobalFormBinding,
    pub info: Value,
    pub state: FormState,
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn global_form(store: &EventStore, id: &FormId) -> Result<Option<GlobalFormRecord>, FormError> {
    let mut stmt = store.connection().prepare(
        "SELECT CASE WHEN length(status) <= 16 THEN status END,
            CASE WHEN suspended_reason IS NULL THEN NULL WHEN length(suspended_reason) <= 16 THEN suspended_reason ELSE '!oversized!' END,
            CASE WHEN octet_length(global_binding) <= 8192 THEN global_binding END,
            CASE WHEN octet_length(answer) <= 262144 THEN answer END,
            octet_length(answer)
         FROM form_request WHERE id=?1 AND session_id IS NULL")?;
    let mut rows = stmt.query([id.as_str()])?;
    let Some(row) = rows.next()? else {
        let has_fact: bool = store.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family='form' AND aggregate_id=?1)",
            [id.as_str()],
            |row| row.get(0),
        )?;
        if has_fact {
            return Err(FormError::Global(
                "form projection missing for durable facts".into(),
            ));
        }
        return Ok(None);
    };
    let status: String = row.get(0)?;
    let reason: Option<String> = row.get(1)?;
    let binding: String = row.get(2)?;
    let binding = GlobalFormBinding::decode(
        &serde_json::from_str::<Value>(&binding)
            .map_err(|error| FormError::Global(error.to_string()))?,
    )
    .map_err(FormError::Global)?;
    let answer: Option<String> = row.get(3)?;
    let answer_len: Option<i64> = row.get(4)?;
    if answer_len.is_some_and(|size| size > 262144) {
        return Err(FormError::Global("answer exceeds bound".into()));
    }
    let suspension = match reason.as_deref() {
        None => None,
        Some("shutdown") => Some(FormSuspensionReason::Shutdown),
        Some("process-loss") => Some(FormSuspensionReason::ProcessLoss),
        _ => return Err(FormError::Global("unknown suspension reason".into())),
    };
    let state = match status.as_str() {
        "pending" if suspension.is_none() && answer.is_none() => FormState::Pending,
        "suspended" if answer.is_none() => FormState::Suspended(
            suspension.ok_or_else(|| FormError::Global("suspension reason absent".into()))?,
        ),
        "answered" => FormState::Answered(
            serde_json::from_str(
                answer
                    .as_deref()
                    .ok_or_else(|| FormError::Global("answer absent".into()))?,
            )
            .map_err(|error| FormError::Global(error.to_string()))?,
        ),
        "cancelled" if answer.is_none() => FormState::Cancelled,
        _ => return Err(FormError::Global("invalid state/answer pair".into())),
    };
    let mut events = store.connection().prepare(
        "SELECT CASE WHEN octet_length(data) <= 262144 THEN data END
         FROM event WHERE aggregate_family='form' AND aggregate_id=?1 AND type='form.created.2' ORDER BY seq LIMIT 2")?;
    let mut events = events.query([id.as_str()])?;
    let event = events
        .next()?
        .ok_or_else(|| FormError::Global("creation fact missing".into()))?;
    let raw: String = event.get(0)?;
    let data: Value =
        serde_json::from_str(&raw).map_err(|error| FormError::Global(error.to_string()))?;
    if events.next()?.is_some() {
        return Err(FormError::Global("duplicate creation facts".into()));
    }
    if data["formID"].as_str() != Some(id.as_str())
        || data["form"]["id"].as_str() != Some(id.as_str())
        || !GlobalFormBinding::decode(&data["owner"])
            .map_err(FormError::Global)?
            .compatible_with(&binding)
    {
        return Err(FormError::Global(
            "creation fact disagrees with projection".into(),
        ));
    }
    let info = data
        .get("form")
        .cloned()
        .ok_or_else(|| FormError::Global("form absent".into()))?;
    if info["sessionID"] != "global" || info["title"].as_str().is_none() {
        return Err(FormError::Global("global form display identity".into()));
    }
    if let Some(error) = crate::answer::validate_fields(&info["fields"]) {
        return Err(FormError::Global(error));
    }
    if let FormState::Answered(answer) = &state {
        if !answer.is_object() {
            return Err(FormError::Global("global answer must be an object".into()));
        }
        if let Some(error) = crate::answer::validate_answer(&info["fields"], answer) {
            return Err(FormError::Global(error));
        }
    }
    Ok(Some(GlobalFormRecord {
        id: id.clone(),
        binding,
        info,
        state,
    }))
}

pub fn pending_global_forms(store: &EventStore) -> Result<Vec<GlobalFormRecord>, FormError> {
    let ids = {
        let mut stmt = store.connection().prepare("SELECT CASE WHEN octet_length(id) <= 512 THEN id END FROM form_request
            WHERE session_id IS NULL AND status IN ('pending','suspended') ORDER BY rowid LIMIT 129")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if ids.len() > 128 {
        return Err(FormError::Global("pending global form row bound".into()));
    }
    let mut records = Vec::new();
    let mut bytes = 0usize;
    for id in ids {
        let id = id
            .parse()
            .map_err(|_| FormError::Global("invalid form identity".into()))?;
        let record = global_form(store, &id)?
            .ok_or_else(|| FormError::Global("listed form disappeared".into()))?;
        bytes = bytes.saturating_add(record.info.to_string().len());
        if bytes > 4 * 1024 * 1024 {
            return Err(FormError::Global("global form page byte bound".into()));
        }
        records.push(record);
    }
    Ok(records)
}

/// Every pending or suspended global form a startup recovery pass can read.
///
/// Unlike [`pending_global_forms`], a form whose durable record cannot be
/// trusted is skipped and named instead of failing the read, so one bad form
/// cannot stop the server from starting. Pending and suspended forms each get
/// their own row bound, so damaged rows of one status never displace the
/// other's healthy forms; rows past a bound wait for the next pass, and a
/// pass suspends the pending forms it reads, so later ones come up in turn.
/// A database failure still fails the read.
pub fn pending_global_forms_for_recovery(
    store: &EventStore,
) -> Result<(Vec<GlobalFormRecord>, Vec<SkippedForm>), FormError> {
    let mut records = Vec::new();
    let mut skipped = Vec::new();
    for status in ["pending", "suspended"] {
        recover_status(store, status, &mut records, &mut skipped)?;
    }
    Ok((records, skipped))
}

/// One status's share of a recovery pass: at most 128 forms, oldest first.
fn recover_status(
    store: &EventStore,
    status: &str,
    records: &mut Vec<GlobalFormRecord>,
    skipped: &mut Vec<SkippedForm>,
) -> Result<(), FormError> {
    let ids = {
        let mut stmt = store.connection().prepare(
            "SELECT CASE WHEN octet_length(id) <= 512 THEN id END FROM form_request
            WHERE session_id IS NULL AND status = ?1 ORDER BY rowid LIMIT 129",
        )?;
        // An identity stored as another type is that row's damage, named
        // below, never a failure of the whole read.
        let rows = stmt.query_map([status], |row| {
            Ok(match row.get_ref(0)? {
                rusqlite::types::ValueRef::Text(text) => {
                    std::str::from_utf8(text).ok().map(str::to_owned)
                }
                _ => None,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (index, raw) in ids.into_iter().enumerate() {
        if index == 128 {
            skipped.push(SkippedForm {
                id: None,
                reason: format!(
                    "more {status} global forms than one recovery pass reads; the rest wait for the next start"
                ),
            });
            break;
        }
        let Some(id) = raw.and_then(|raw| raw.parse::<FormId>().ok()) else {
            skipped.push(SkippedForm {
                id: None,
                reason: "global form identity is absent, oversized or invalid".into(),
            });
            continue;
        };
        match global_form(store, &id) {
            Ok(Some(record)) => records.push(record),
            Ok(None) => skipped.push(SkippedForm {
                id: Some(id),
                reason: "listed form disappeared".into(),
            }),
            Err(error) if error.is_storage_failure() => return Err(error),
            Err(error) => skipped.push(SkippedForm {
                id: Some(id),
                reason: error.to_string(),
            }),
        }
    }
    Ok(())
}

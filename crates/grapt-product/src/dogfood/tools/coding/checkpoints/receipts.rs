//! Canonical patch receipts remain visible after conversation rewind.
use super::{
    attribution::{AppliedPatch, UndoSelection},
    manifest, FileContext,
};
use grapt_artifact::FilesystemArtifactStore;
use grapt_event::session_patch::{self, PatchReceiptFact};
use grapt_store::EventStore;
use grapt_types::{DeterministicAuthorities, MessageId, SessionId, ToolTerminalArtifact};
use std::collections::BTreeSet;
use std::path::Path;

pub(super) fn record(context: &FileContext<'_>, receipt: &AppliedPatch) -> Result<(), String> {
    let bytes = serde_json::to_vec(&receipt.encode()?).map_err(|error| error.to_string())?;
    let reference = manifest::capture(context.artifacts, &bytes, session_patch::MAX_RECEIPT_BYTES)?;
    let artifact = ToolTerminalArtifact::new(reference, grapt_types::CaptureDisposition::Complete)
        .ok_or("patch artifact is not complete")?;
    let fact = PatchReceiptFact::new(
        context.session.clone(),
        context.input.clone(),
        context.call.clone(),
        artifact,
    )
    .map_err(|error| error.to_string())?;
    let mut inbox = context
        .inbox
        .lock()
        .map_err(|_| "patch receipt inbox owner is unavailable")?;
    let authorities = DeterministicAuthorities::new(&*context.clock, &*context.ids);
    grapt_session::patch_receipt::record(inbox.store_mut(), &authorities, &fact)
}

fn fact(kind: &str, raw: Option<String>, session: &SessionId) -> Result<PatchReceiptFact, String> {
    if kind != "session.patch.receipt.1" {
        return Err("patch receipt has an unsupported version".into());
    }
    let raw = raw.ok_or("patch receipt exceeds its event read bound")?;
    let value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    let fact = session_patch::decode(&value).map_err(|error| error.to_string())?;
    if fact.session() != session {
        return Err("patch receipt belongs to another Session".into());
    }
    Ok(fact)
}

pub(in crate::dogfood) fn select(
    store: &EventStore,
    artifacts: &FilesystemArtifactStore,
    root: &Path,
    session: &SessionId,
) -> Result<UndoSelection, String> {
    let conn = store.connection();
    let mut query = conn
        .prepare(
            "SELECT type, CASE WHEN length(CAST(data AS BLOB)) <= 8192 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type LIKE 'session.patch.receipt.%' ORDER BY seq DESC LIMIT 1",
        )
        .map_err(|error| error.to_string())?;
    let mut rows = query
        .query([session.as_str()])
        .map_err(|error| error.to_string())?;
    let row = rows
        .next()
        .map_err(|error| error.to_string())?
        .ok_or("this Session has no recorded patch files to undo")?;
    let kind: String = row.get(0).map_err(|error| error.to_string())?;
    let raw: Option<String> = row.get(1).map_err(|error| error.to_string())?;
    let input: MessageId = fact(&kind, raw, session)?.input().clone();
    select_input(store, artifacts, root, session, &input)
}

pub(super) fn select_input(
    store: &EventStore,
    artifacts: &FilesystemArtifactStore,
    root: &Path,
    session: &SessionId,
    input: &MessageId,
) -> Result<UndoSelection, String> {
    let conn = store.connection();
    let mut query = conn
        .prepare(
            "SELECT type, CASE WHEN length(CAST(data AS BLOB)) <= 8192 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type LIKE 'session.patch.receipt.%' AND CASE WHEN json_valid(data) = 0 THEN 1
         WHEN json_type(data, '$.inputID') IS NOT 'text' THEN 1
         ELSE json_extract(data, '$.inputID') = ?2 END ORDER BY seq LIMIT 129",
        )
        .map_err(|error| error.to_string())?;
    let mut rows = query
        .query((session.as_str(), input.as_str()))
        .map_err(|error| error.to_string())?;
    let mut calls = BTreeSet::new();
    let mut receipts = Vec::new();
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        if receipts.len() == 128 {
            return Err("turn exceeds its 128-patch undo bound".into());
        }
        let kind: String = row.get(0).map_err(|error| error.to_string())?;
        let raw = row.get(1).map_err(|error| error.to_string())?;
        let fact = fact(&kind, raw, session)?;
        if fact.input() != input || !calls.insert(fact.call().clone()) {
            return Err("patch receipt has foreign or duplicate call coordinates".into());
        }
        let bytes = manifest::fetch(
            artifacts,
            fact.receipt().reference(),
            session_patch::MAX_RECEIPT_BYTES,
        )?;
        let value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        receipts.push(AppliedPatch::decode(&value)?);
    }
    super::attribution::select(artifacts, root, input, &receipts)
}

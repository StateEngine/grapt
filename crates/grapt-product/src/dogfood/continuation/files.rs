//! Final attachments derive from committed child receipts, never guest IDs.
use super::*;
use grapt_types::{ArtifactDigest, ArtifactId, ArtifactRef};

pub(in crate::dogfood) struct FinalFile {
    pub reference: ArtifactRef,
    pub mime: String,
}
impl std::fmt::Debug for FinalFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FinalFile(<redacted>)")
    }
}

pub(in crate::dogfood) fn collect(
    store: &EventStore,
    artifacts: &FilesystemArtifactStore,
    session: &SessionId,
    parent: &ToolCallId,
) -> Result<Vec<FinalFile>, String> {
    let mut children = Vec::new();
    for (_, data) in scoped_rows(
        store,
        session,
        "session.tool.nested.started.1",
        None,
        None,
        Scope::Call(Some(parent), "$.parent"),
        MAX_CONTINUATION_ROWS,
    )? {
        let value = serde_json::from_str(&data).map_err(|_| "nested admission is not JSON")?;
        let fact = grapt_event::session_tool_nested::decode(&value)
            .map_err(|_| "nested admission corrupt")?;
        if fact.parent() == parent {
            let key = (fact.attempt().clone(), fact.call().clone());
            if children.contains(&key) || children.len() >= 64 {
                return Err("nested attachment admission bound or duplicate".into());
            }
            children.push(key);
        }
    }
    if children.is_empty() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let mut settled = std::collections::BTreeSet::new();
    let mut bytes = 0u64;
    for ScopedTerminal { terminal: data, .. } in scoped_terminals(
        store,
        session,
        None,
        None,
        Scope::Composition(parent),
        MAX_CONTINUATION_ROWS,
    )? {
        let StoredTerminal::Readable(fact) = data else {
            continue;
        };
        let Some(ordinal) = children
            .iter()
            .position(|key| key == &(fact.attempt().clone(), fact.call().clone()))
        else {
            continue;
        };
        if !settled.insert(ordinal) {
            return Err("nested attachment terminal duplicated".into());
        }
        let ToolTerminalOutcome::Completed {
            output: Some(output),
        } = fact.outcome()
        else {
            continue;
        };
        let label = "nested machine output";
        let raw = match fetch_bytes(artifacts, output.reference(), output.disposition(), label)? {
            Ok(raw) => raw,
            // An output the store cannot produce costs only its own files.
            Err(error) => {
                crate::diagnostics::report(format!(
                    "{label} of {} is unreadable; its files are omitted: {error}",
                    fact.call()
                ));
                continue;
            }
        };
        for file in listed_files(&raw, fact.call()) {
            bytes = bytes.saturating_add(file.reference.length());
            if files.len() >= 32 || bytes > 512 * 1024 {
                return Err("final attachment aggregation bound exceeded".into());
            }
            let metadata = artifacts
                .stat(file.reference.id())
                .map_err(|_| "attachment metadata unreadable")?;
            if metadata.digest() != file.reference.digest()
                || metadata.length() != file.reference.length()
                || !metadata.disposition().is_complete()
                || !metadata.sensitivity().is_secret_bearing()
            {
                return Err("attachment provenance disagrees".into());
            }
            files.push((ordinal, file));
        }
    }
    files.sort_by_key(|(ordinal, _)| *ordinal);
    Ok(files.into_iter().map(|(_, file)| file).collect())
}

/// The files one nested machine output lists. A malformed output, like an
/// unreadable one, costs only its own files, and a malformed entry only itself.
fn listed_files(raw: &[u8], call: &ToolCallId) -> Vec<FinalFile> {
    let label = "nested machine output";
    let value = serde_json::from_slice::<serde_json::Value>(raw).ok();
    let Some(rows) = value
        .as_ref()
        .and_then(|value| value.get("artifacts"))
        .and_then(serde_json::Value::as_array)
    else {
        crate::diagnostics::report(format!(
            "{label} of {call} is malformed; its files are omitted"
        ));
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            decode(row)
                .map_err(|error| {
                    crate::diagnostics::report(format!(
                        "{label} of {call} lists a malformed file, which is omitted: {error}"
                    ));
                })
                .ok()
        })
        .collect()
}

pub(in crate::dogfood) fn content(
    file: &FinalFile,
    store: &FilesystemArtifactStore,
) -> Result<String, String> {
    use base64::Engine as _;
    let budget = ReadBudget::new(64 * 1024).ok_or("attachment read bound")?;
    let mut bytes = Vec::new();
    let mut offset = 0;
    loop {
        let page = store
            .fetch(&file.reference, offset, budget)
            .map_err(|_| "attachment bytes unreadable")?;
        if bytes.len().saturating_add(page.bytes().len()) > 64 * 1024 {
            return Err("attachment read bound exceeded".into());
        }
        bytes.extend_from_slice(page.bytes());
        match page.next_offset() {
            Some(next) if next > offset => offset = next,
            None => break,
            _ => return Err("attachment read failed to advance".into()),
        }
    }
    if bytes.len() as u64 != file.reference.length() {
        return Err("attachment length disagrees".into());
    }
    let uri = format!(
        "data:{};base64,{}",
        file.mime,
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );
    let uri = serde_json::to_string(&uri).map_err(|_| "attachment URI encoding")?;
    let mime = serde_json::to_string(&file.mime).map_err(|_| "attachment MIME encoding")?;
    let name = serde_json::to_string(file.reference.id().as_str())
        .map_err(|_| "attachment name encoding")?;
    Ok(format!(
        "{{\"type\":\"file\",\"uri\":{uri},\"mime\":{mime},\"name\":{name}}}"
    ))
}

fn decode(value: &serde_json::Value) -> Result<FinalFile, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        id: String,
        digest: String,
        length: u64,
        #[serde(rename = "mimeType")]
        mime: String,
        sensitivity: String,
    }
    let wire: Wire =
        serde_json::from_value(value.clone()).map_err(|_| "attachment reference corrupt")?;
    if wire.id.len() > 256
        || wire.digest.len() > 128
        || wire.sensitivity != "secret-bearing"
        || wire.length > 64 * 1024
        || wire.mime.is_empty()
        || wire.mime.len() > 256
    {
        return Err("attachment reference exceeds bounds or loses sensitivity".into());
    }
    Ok(FinalFile {
        reference: ArtifactRef::new(
            ArtifactId::new(wire.id).ok_or("attachment id invalid")?,
            wire.length,
            ArtifactDigest::new(wire.digest).ok_or("attachment digest invalid")?,
        ),
        mime: wire.mime,
    })
}

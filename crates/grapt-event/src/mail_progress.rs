//! Local durable page receipt. Integrity gaps are observations, never mailbox
//! admissions; a checkpoint names a fully recorded page, not handled mail.
use grapt_types::remote_mail::{progress::*, *};
use grapt_types::{MailboxAdmissionSeq, MailboxMessageId, SeatRecipientId};
use serde_json::{json, Value};

pub const MAX_MAIL_PROGRESS_BYTES: usize = 262_144;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailStream {
    pub projects: ProjectPair,
    pub principal: SourcePrincipalId,
    pub recipient: SeatRecipientId,
    pub protocol: MailTransportVersion,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMail {
    pub local_message: MailboxMessageId,
    pub admission: MailboxAdmissionSeq,
    pub digest: MailboxMessageDigest,
    pub enumerated_at: ExclusiveSourceCursor,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailPageRecorded {
    pub stream: MailStream,
    pub previous: Option<ExclusiveSourceCursor>,
    pub checkpoint: ExclusiveSourceCursor,
    messages: Vec<RecordedMail>,
    gaps: Vec<RefusedMailRow>,
    decode_failures: Vec<MailDecodeFailure>,
}
impl MailPageRecorded {
    pub fn new(
        stream: MailStream,
        previous: Option<ExclusiveSourceCursor>,
        checkpoint: ExclusiveSourceCursor,
        messages: Vec<RecordedMail>,
        gaps: Vec<RefusedMailRow>,
    ) -> Result<Self, String> {
        Self::new_with_decode_failures(stream, previous, checkpoint, messages, gaps, Vec::new())
    }
    pub fn new_with_decode_failures(
        stream: MailStream,
        previous: Option<ExclusiveSourceCursor>,
        checkpoint: ExclusiveSourceCursor,
        messages: Vec<RecordedMail>,
        gaps: Vec<RefusedMailRow>,
        decode_failures: Vec<MailDecodeFailure>,
    ) -> Result<Self, String> {
        let count = messages
            .len()
            .saturating_add(gaps.len())
            .saturating_add(decode_failures.len());
        if count == 0
            || count > 64
            || checkpoint.as_str().len() > 144
            || previous.as_ref() == Some(&checkpoint)
            || previous.as_ref().is_some_and(|c| c.as_str().len() > 144)
            || (stream.protocol == MailTransportVersion::V1
                && (!gaps.is_empty() || !decode_failures.is_empty()))
        {
            return Err("mail page progress bounds or context".into());
        }
        let mut local = std::collections::BTreeSet::new();
        if messages.iter().any(|m| !local.insert(&m.local_message))
            || gaps.windows(2).any(|rows| rows[0].end() > rows[1].start())
        {
            return Err("mail page repeats admissions or integrity coordinates".into());
        }
        let mut cursors = std::collections::BTreeSet::new();
        for cursor in messages
            .iter()
            .map(|m| &m.enumerated_at)
            .chain(gaps.iter().map(|g| g.cursor()))
            .chain(decode_failures.iter().map(|f| f.cursor()))
        {
            if !cursors.insert(cursor) {
                return Err("duplicate page position".into());
            }
        }
        if messages.last().map(|m| &m.enumerated_at) != Some(&checkpoint)
            && gaps.last().map(|g| g.cursor()) != Some(&checkpoint)
            && decode_failures.last().map(|f| f.cursor()) != Some(&checkpoint)
        {
            return Err("checkpoint is not a final emitted page position".into());
        }
        Ok(Self {
            stream,
            previous,
            checkpoint,
            messages,
            gaps,
            decode_failures,
        })
    }
    pub fn messages(&self) -> &[RecordedMail] {
        &self.messages
    }
    pub fn gaps(&self) -> &[RefusedMailRow] {
        &self.gaps
    }
    pub fn decode_failures(&self) -> &[MailDecodeFailure] {
        &self.decode_failures
    }
    pub fn encode(&self) -> Value {
        // Keep the exact existing payload/hash when there are no local failures.
        let mut value = json!({"receiving":{"source":self.stream.projects.source().as_str(),"target":self.stream.projects.target().as_str(),
            "principal":self.stream.principal.as_str(),"recipient":self.stream.recipient.as_str()},
            "transport":self.stream.protocol.as_str(),"previous":self.previous.as_ref().map(|c|c.as_str()),"checkpoint":self.checkpoint.as_str(),
            "admissions":self.messages.iter().map(|m|json!({"local_message":m.local_message.as_str(),"admission":m.admission.value(),"digest":m.digest.as_str(),"position":m.enumerated_at.as_str()})).collect::<Vec<_>>(),
            "integrity_gaps":self.gaps.iter().map(|g|json!({"indexed_label":g.indexed_id().map(|i|i.as_str()),"byte_range":[g.start(),g.end()],"cause":g.reason().as_str(),"position":g.cursor().as_str()})).collect::<Vec<_>>()});
        if !self.decode_failures.is_empty() {
            value["decode_failures"] =
                json!(self.decode_failures.iter().map(|failure|
                json!({"cause":"unknown-kind","position":failure.cursor().as_str()}))
                .collect::<Vec<_>>());
        }
        value
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn decode(value: &Value) -> Result<Self, String> {
        exact(
            value,
            if value.get("decode_failures").is_some() {
                &[
                    "receiving",
                    "transport",
                    "previous",
                    "checkpoint",
                    "admissions",
                    "integrity_gaps",
                    "decode_failures",
                ]
            } else {
                &[
                    "receiving",
                    "transport",
                    "previous",
                    "checkpoint",
                    "admissions",
                    "integrity_gaps",
                ]
            },
        )?;
        let scope = &value["receiving"];
        exact(scope, &["source", "target", "principal", "recipient"])?;
        let stream = MailStream {
            projects: ProjectPair::new(
                SourceProjectId::try_new(text(scope, "source")?).map_err(|e| e.to_string())?,
                TargetProjectId::try_new(text(scope, "target")?).map_err(|e| e.to_string())?,
            ),
            principal: SourcePrincipalId::try_new(text(scope, "principal")?)
                .map_err(|e| e.to_string())?,
            recipient: SeatRecipientId::try_new(text(scope, "recipient")?)
                .map_err(|e| e.to_string())?,
            protocol: MailTransportVersion::parse(text(value, "transport")?)
                .ok_or("mail transport context")?,
        };
        let previous = if value["previous"].is_null() {
            None
        } else {
            Some(
                ExclusiveSourceCursor::try_new(text(value, "previous")?)
                    .map_err(|e| e.to_string())?,
            )
        };
        let checkpoint = ExclusiveSourceCursor::try_new(text(value, "checkpoint")?)
            .map_err(|e| e.to_string())?;
        let mut messages = Vec::new();
        for v in array(value, "admissions")? {
            exact(v, &["local_message", "admission", "digest", "position"])?;
            messages.push(RecordedMail {
                local_message: MailboxMessageId::try_new(text(v, "local_message")?)
                    .map_err(|e| e.to_string())?,
                admission: MailboxAdmissionSeq::new(number(v, "admission")?).ok_or("admission")?,
                digest: MailboxMessageDigest::try_new(text(v, "digest")?)
                    .map_err(|e| e.to_string())?,
                enumerated_at: ExclusiveSourceCursor::try_new(text(v, "position")?)
                    .map_err(|e| e.to_string())?,
            });
        }
        let mut gaps = Vec::new();
        for v in array(value, "integrity_gaps")? {
            exact(v, &["indexed_label", "byte_range", "cause", "position"])?;
            let id = if v["indexed_label"].is_null() {
                None
            } else {
                Some(
                    RefusedIndexId::new(text(v, "indexed_label")?.to_owned())
                        .map_err(|e| e.to_string())?,
                )
            };
            let range = v["byte_range"]
                .as_array()
                .filter(|a| a.len() == 2)
                .ok_or("gap range")?;
            let start = range[0].as_u64().ok_or("gap start")?;
            let end = range[1].as_u64().ok_or("gap end")?;
            let reason = RefusedRowReason::parse(text(v, "cause")?).ok_or("gap cause")?;
            let cursor =
                ExclusiveSourceCursor::try_new(text(v, "position")?).map_err(|e| e.to_string())?;
            gaps.push(RefusedMailRow::new(id, start, end, reason, cursor).ok_or("gap bounds")?);
        }
        let mut failures = Vec::new();
        if value.get("decode_failures").is_some() {
            let rows = array(value, "decode_failures")?;
            if rows.is_empty() {
                return Err("empty local decode failure extension".into());
            }
            for row in rows {
                exact(row, &["cause", "position"])?;
                if text(row, "cause")? != "unknown-kind" {
                    return Err("unknown local decode failure".into());
                }
                failures.push(
                    MailDecodeFailure::unknown_kind(
                        ExclusiveSourceCursor::try_new(text(row, "position")?)
                            .map_err(|e| e.to_string())?,
                    )
                    .ok_or("local decode failure cursor bound")?,
                );
            }
        }
        Self::new_with_decode_failures(stream, previous, checkpoint, messages, gaps, failures)
    }
}
fn exact(value: &Value, fields: &[&str]) -> Result<(), String> {
    let object = value.as_object().ok_or("mail progress object")?;
    if object.len() != fields.len() || fields.iter().any(|key| !object.contains_key(*key)) {
        return Err("mail progress shape".into());
    }
    Ok(())
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| "mail progress text".into())
}
fn number(value: &Value, key: &str) -> Result<u64, String> {
    value[key]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
        .ok_or_else(|| "mail progress integer".into())
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], String> {
    value[key]
        .as_array()
        .filter(|a| a.len() <= 64)
        .map(Vec::as_slice)
        .ok_or_else(|| "mail progress count".into())
}

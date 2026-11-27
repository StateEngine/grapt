//! Global form facts use the ordinary fenced append and projection transaction.
//! The session-only legacy decoder remains unchanged.
use crate::{DecodeError, DecodedEvent, Projector};
use grapt_event::EventKind;
use grapt_types::{EventId, EventSeq, FormId};
use serde_json::Value;

pub const MAX_FORM_FACT_BYTES: usize = 256 * 1024;

pub(crate) fn bound_input(value: &Value) -> Result<(), DecodeError> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_FORM_FACT_BYTES.saturating_sub(self.0) {
                return Err(std::io::Error::other("global form fact byte bound"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(0), value)
        .map_err(|_| DecodeError::Form("global form fact byte bound"))
}

#[derive(Debug, Clone)]
pub struct DecodedFormFact {
    pub id: EventId,
    pub form: FormId,
    pub seq: EventSeq,
    pub created_ms: i64,
    pub kind: EventKind,
    pub data: Value,
}

pub(crate) enum ProjectionFact {
    Session(DecodedEvent),
    Form(DecodedFormFact),
    Mail(crate::mailbox::remote::progress::DecodedMailProgress),
}

impl ProjectionFact {
    pub(crate) fn seq(&self) -> EventSeq {
        match self {
            Self::Session(event) => event.seq,
            Self::Form(event) => event.seq,
            Self::Mail(event) => event.seq,
        }
    }
    pub(crate) fn kind(&self) -> EventKind {
        match self {
            Self::Session(event) => event.kind,
            Self::Form(event) => event.kind,
            Self::Mail(_) => EventKind::MailPageRecordedV1,
        }
    }
    pub(crate) fn data(&self) -> &Value {
        match self {
            Self::Session(event) => &event.data,
            Self::Form(event) => &event.data,
            Self::Mail(event) => &event.data,
        }
    }
    pub(crate) fn project(
        &self,
        projector: &mut dyn Projector,
        tx: &rusqlite::Transaction<'_>,
    ) -> Result<(), String> {
        match self {
            Self::Session(event) => projector.project(tx, event),
            Self::Form(event) => projector.project_form(tx, event),
            Self::Mail(_) => Err("mail progress has no external projector".into()),
        }
    }
}

pub(crate) fn decode_fact(
    id: &str,
    family: &str,
    aggregate: &str,
    seq: i64,
    created_ms: i64,
    kind: &str,
    data: &str,
) -> Result<ProjectionFact, DecodeError> {
    match family {
        "session" => {
            let event = crate::decode::decode_row(id, aggregate, seq, created_ms, kind, data)?;
            if event.kind.is_global_form() || event.kind.is_native_mail() {
                return Err(DecodeError::Form("global form kind on session aggregate"));
            }
            Ok(ProjectionFact::Session(event))
        }
        "form" => {
            if data.len() > MAX_FORM_FACT_BYTES {
                return Err(DecodeError::Form("payload byte bound"));
            }
            let kind = EventKind::parse(kind).map_err(|_| DecodeError::Form("unknown kind"))?;
            if !kind.is_global_form() {
                return Err(DecodeError::Form("non-form kind"));
            }
            let form: FormId = aggregate
                .parse()
                .map_err(|_| DecodeError::Form("form identity"))?;
            let data: Value =
                serde_json::from_str(data).map_err(|_| DecodeError::Form("payload JSON"))?;
            if data.get("formID").and_then(Value::as_str) != Some(form.as_str()) {
                return Err(DecodeError::Form("payload owner disagrees with aggregate"));
            }
            Ok(ProjectionFact::Form(DecodedFormFact {
                id: id
                    .parse()
                    .map_err(|_| DecodeError::Form("event identity"))?,
                form,
                seq: EventSeq::new(
                    u64::try_from(seq).map_err(|_| DecodeError::Form("negative sequence"))?,
                ),
                created_ms,
                kind,
                data,
            }))
        }
        "mail" => crate::mailbox::remote::progress::decode_fact(
            id, aggregate, seq, created_ms, kind, data,
        )
        .map(ProjectionFact::Mail),
        _ => Err(DecodeError::Form("unsupported aggregate family")),
    }
}

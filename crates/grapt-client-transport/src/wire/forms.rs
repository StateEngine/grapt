//! Forms: the question, its state union, and settling it.
//!
//! Split from `wire.rs` by SUBJECT when that file passed this workspace's
//! module ceiling. Every encode and decode still goes through the real codecs
//! under the real limits; nothing here was rewritten on the way out.

use super::catalogs::location_value;
use super::*;

mod notification;
pub use notification::{global_form_changed, session_form_changed};

/// One pending form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormRow {
    pub id: FormWireId,
    pub owner: FormOwner,
    pub title: String,
    pub fields: Vec<FormField>,
    pub metadata: std::collections::BTreeMap<String, serde_json::Value>,
}

/// The resolved location at which an MCP question was discovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormLocation {
    pub project: ProjectWireId,
    pub directory: String,
}

/// Read the fields of one `Form.Info`, through its real serialization.
///
/// `FormFieldDto` is an open object with no accessors, so the honest reading is
/// to serialize it back through the REAL codec and read the members the frozen
/// validator already proved are there. A field whose shape the validator would
/// refuse never reaches this function: the decode failed first.
fn form_fields(info: &grapt_api::u3a::FormInfoDto) -> Result<Vec<FormField>, WireError> {
    let value = serde_json::to_value(info.fields().get()).map_err(|error| WireError::Invalid {
        what: "form fields",
        value: error.to_string(),
    })?;
    let entries = value.as_array().ok_or_else(|| WireError::Invalid {
        what: "form fields",
        value: "the frozen record declares an array".to_owned(),
    })?;
    let mut fields = Vec::with_capacity(entries.len());
    for entry in entries {
        // `key` and `type` are REQUIRED by the frozen validator, so their
        // absence here is a contract violation rather than an optional value.
        let key = entry
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| WireError::Invalid {
                what: "form field",
                value: "a field without a key".to_owned(),
            })?;
        let kind = entry
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| WireError::Invalid {
                what: "form field",
                value: "a field without a type".to_owned(),
            })?;
        fields.push(FormField {
            attributes: entry
                .as_object()
                .ok_or_else(|| WireError::Invalid {
                    what: "form field",
                    value: "expected field object".into(),
                })?
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "key" | "type" | "title" | "required"))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            key: key.to_owned(),
            kind: kind.to_owned(),
            title: entry
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            // Absent means not required, which is the frozen default rather
            // than a guess: an external field cannot carry `required` at all.
            required: entry
                .get("required")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(fields)
}

fn form_row(info: &grapt_api::u3a::FormInfoDto) -> Result<FormRow, WireError> {
    Ok(FormRow {
        metadata: info
            .metadata()
            .as_present()
            .map(|metadata| {
                serde_json::to_value(metadata).and_then(
                    serde_json::from_value::<std::collections::BTreeMap<String, serde_json::Value>>,
                )
            })
            .transpose()
            .map_err(|error| WireError::Invalid {
                what: "form metadata",
                value: error.to_string(),
            })?
            .unwrap_or_default(),
        id: info.id().get().clone(),
        owner: {
            let spelled = info.session_id().get().as_str();
            match spelled {
                "global" => FormOwner::Global,
                _ => match session_id(spelled) {
                    Ok(session) => FormOwner::Session(session),
                    // Preserve unknown owners for display without granting them
                    // the global elicitation address.
                    Err(_) => FormOwner::Unattributed(spelled.to_owned()),
                },
            }
        },
        title: info.title().get().as_str().to_owned(),
        fields: form_fields(info)?,
    })
}

/// Decode a real `FormListResponseDto` — pending forms across sessions.
pub fn decode_form_requests(bytes: &[u8]) -> Result<Vec<FormRow>, WireError> {
    use grapt_api::u3a::FormListResponseDto;
    let dto: FormListResponseDto = decode_page(bytes, limits()?)?;
    dto.data().get().as_slice().iter().map(form_row).collect()
}

pub fn decode_location_forms(bytes: &[u8]) -> Result<(FormLocation, Vec<FormRow>), WireError> {
    use grapt_api::u3a::FormListResponseDto;
    let dto: FormListResponseDto = decode_page(bytes, limits()?)?;
    let location = dto.location().get();
    Ok((
        FormLocation {
            project: location.project().get().id().get().clone(),
            directory: location.directory().get().as_str().to_owned(),
        },
        dto.data()
            .get()
            .as_slice()
            .iter()
            .map(form_row)
            .collect::<Result<_, _>>()?,
    ))
}

/// Decode a real `SessionFormListResponseDto` — pending forms in one session.
pub fn decode_session_forms(bytes: &[u8]) -> Result<Vec<FormRow>, WireError> {
    use grapt_api::u3a::SessionFormListResponseDto;
    let dto: SessionFormListResponseDto = decode_page(bytes, limits()?)?;
    form_rows(dto.data().get().as_slice())
}

/// The one projection of typed pending forms, whichever response carried them.
pub fn form_rows(rows: &[grapt_api::u3a::FormInfoDto]) -> Result<Vec<FormRow>, WireError> {
    rows.iter().map(form_row).collect()
}

/// Encode a real `FormListResponseDto`, validated by the real decoder.
pub fn encode_form_requests(location: &ProjectRow, rows: &[FormRow]) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::FormListResponseDto;
    let bytes = serde_json::json!({
        "location": location_value(location),
        "data": rows.iter().map(form_value).collect::<Vec<_>>(),
    })
    .to_string()
    .into_bytes();
    let _validated: FormListResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Encode a real `SessionFormListResponseDto`, validated by the real decoder.
pub fn encode_session_forms(rows: &[FormRow]) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::SessionFormListResponseDto;
    let bytes = serde_json::json!({
        "data": rows.iter().map(form_value).collect::<Vec<_>>(),
    })
    .to_string()
    .into_bytes();
    let _validated: SessionFormListResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Canonical form values, preserving JSON numbers and string lists as typed values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormValue {
    Text(String),
    Flag(bool),
    Number(serde_json::Number),
    Texts(Vec<String>),
}

impl FormValue {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Text(text) => serde_json::Value::String(text.clone()),
            Self::Flag(flag) => serde_json::Value::Bool(*flag),
            Self::Number(number) => serde_json::Value::Number(number.clone()),
            Self::Texts(items) => serde_json::Value::Array(
                items
                    .iter()
                    .cloned()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        }
    }
}

/// Why a form was suspended, exactly as the frozen record spells the reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormSuspension {
    Shutdown,
    ProcessLoss,
}

impl FormSuspension {
    pub fn as_str(self) -> &'static str {
        match self {
            FormSuspension::Shutdown => "shutdown",
            FormSuspension::ProcessLoss => "process-loss",
        }
    }
}

/// A form's settlement state — the frozen `Form.State` union, all four members.
///
/// `Suspended` is a MEMBER, not a flavour of cancelled, and that distinction is
/// the whole reason this type is a union rather than a bool. A suspended form
/// was interrupted by shutdown or a lost process; it is nonterminal, still
/// listable and still REPLYABLE. A client that folded it into `Cancelled` would
/// present a recoverable question as a dead one and give the user no way back
/// to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormState {
    Pending,
    /// Carries the answer the form was settled with, so a reopened session can
    /// show what was chosen without a second round trip.
    Answered(std::collections::BTreeMap<String, FormValue>),
    Cancelled,
    Suspended(FormSuspension),
}

impl FormState {
    /// Whether this client may still offer to answer the form.
    ///
    /// Pending and suspended are both open. This is the predicate a screen asks,
    /// and it exists so that "can I still answer it?" is decided in ONE place
    /// rather than re-derived per view — where suspended would eventually be
    /// forgotten.
    pub fn is_open(&self) -> bool {
        matches!(self, FormState::Pending | FormState::Suspended(_))
    }
}

fn form_answer_value(value: &FormValue) -> serde_json::Value {
    value.to_json()
}

fn form_answer_object(
    answer: &std::collections::BTreeMap<String, FormValue>,
) -> serde_json::Map<String, serde_json::Value> {
    answer
        .iter()
        .map(|(key, value)| (key.clone(), form_answer_value(value)))
        .collect()
}

/// Read a decoded `FormAnswerDto` without converting values to display strings.
fn form_answer_from(value: &serde_json::Value) -> Result<FormValue, WireError> {
    match value {
        serde_json::Value::String(text) => Ok(FormValue::Text(text.clone())),
        serde_json::Value::Bool(flag) => Ok(FormValue::Flag(*flag)),
        serde_json::Value::Number(number) => Ok(FormValue::Number(number.clone())),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| WireError::Invalid {
                        what: "form string list",
                        value: item.to_string(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(FormValue::Texts),
        other => Err(WireError::Invalid {
            what: "form answer value",
            value: other.to_string(),
        }),
    }
}

/// Encode a real `Form.Reply`, validated by the real decoder.
pub fn encode_form_reply(
    answer: &std::collections::BTreeMap<String, FormValue>,
) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::FormReplyRequestDto;
    let bytes = serde_json::json!({ "answer": form_answer_object(answer) })
        .to_string()
        .into_bytes();
    let _validated: FormReplyRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

/// Decode a real `Form.Reply` — the producer side of a settlement.
pub fn decode_form_reply(
    bytes: &[u8],
) -> Result<std::collections::BTreeMap<String, FormValue>, WireError> {
    use grapt_api::u3a::FormReplyRequestDto;
    let dto: FormReplyRequestDto = decode_request(bytes, limits()?)?;
    let mut answer = std::collections::BTreeMap::new();
    for (key, value) in dto.answer().get().get().as_map() {
        answer.insert(key.as_str().to_owned(), form_answer_from(value.get())?);
    }
    Ok(answer)
}

/// Encode a real `FormStateResponseDto`, validated by the real decoder.
pub fn encode_form_state(state: &FormState) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::FormStateResponseDto;
    // The tag is written FIRST, and that ordering is load-bearing rather than
    // stylistic. `tagged_union!` decodes tag-first BY DESIGN: it reads the
    // leading key and refuses the object outright if that key is not the tag,
    // which is how it dispatches without buffering an arbitrary map to go
    // looking for one. `serde_json`'s object is a `BTreeMap`, so building this
    // through `json!` would SORT the keys and emit `answer` and `reason` ahead
    // of `status` — a payload the real decoder then refuses. Hence the explicit
    // spelling; `decode_request` below is what proves it.
    let data = match state {
        FormState::Pending => r#"{"status":"pending"}"#.to_owned(),
        FormState::Cancelled => r#"{"status":"cancelled"}"#.to_owned(),
        FormState::Suspended(reason) => {
            format!(r#"{{"status":"suspended","reason":"{}"}}"#, reason.as_str())
        }
        FormState::Answered(answer) => format!(
            r#"{{"status":"answered","answer":{}}}"#,
            serde_json::Value::Object(form_answer_object(answer))
        ),
    };
    let bytes = format!(r#"{{"data":{data}}}"#).into_bytes();
    let _validated: FormStateResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Decode a real `FormStateResponseDto`.
pub fn decode_form_state(bytes: &[u8]) -> Result<FormState, WireError> {
    use grapt_api::u3a::{FormStateDto, FormStateResponseDto};
    let dto: FormStateResponseDto = decode_page(bytes, limits()?)?;
    match dto.data().get() {
        FormStateDto::Pending(_) => Ok(FormState::Pending),
        FormStateDto::Answered(answered) => {
            let mut answer = std::collections::BTreeMap::new();
            for (key, value) in answered.answer().get().get().as_map() {
                answer.insert(key.as_str().to_owned(), form_answer_from(value.get())?);
            }
            Ok(FormState::Answered(answer))
        }
        FormStateDto::Cancelled(_) => Ok(FormState::Cancelled),
        FormStateDto::Suspended(suspended) => {
            use grapt_api::u3a::FormSuspendedReasonDto;
            Ok(FormState::Suspended(match suspended.reason().get() {
                FormSuspendedReasonDto::Shutdown => FormSuspension::Shutdown,
                FormSuspendedReasonDto::ProcessLoss => FormSuspension::ProcessLoss,
            }))
        }
    }
}

/// Encode a real `FormResponseDto` — one form, by its own identity.
pub fn encode_form(form: &FormRow) -> Result<Vec<u8>, WireError> {
    use grapt_api::u3a::FormResponseDto;
    let bytes = serde_json::json!({ "data": form_value(form) })
        .to_string()
        .into_bytes();
    let _validated: FormResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}

/// Decode a real `FormResponseDto`.
pub fn decode_form(bytes: &[u8]) -> Result<FormRow, WireError> {
    use grapt_api::u3a::FormResponseDto;
    let dto: FormResponseDto = decode_page(bytes, limits()?)?;
    form_row(dto.data().get())
}

fn form_value(form: &FormRow) -> serde_json::Value {
    let fields: Vec<serde_json::Value> = form
        .fields
        .iter()
        .map(|field| {
            let mut value = serde_json::json!({
                "key": field.key,
                "type": field.kind,
            });
            if let Some(object) = value.as_object_mut() {
                object.extend(field.attributes.clone());
                if let Some(title) = field.title.as_ref() {
                    object.insert("title".to_owned(), serde_json::Value::from(title.as_str()));
                }
                // `required` is OMITTED when false, which is what a producer
                // that never sets it emits and what an `external` field is
                // obliged to do. Writing `false` explicitly instead would mean
                // the reader's absent-is-not-required default was never
                // exercised by anything — and a default nothing exercises is a
                // default nothing catches when it flips.
                if field.required {
                    object.insert("required".to_owned(), serde_json::Value::Bool(true));
                }
            }
            value
        })
        .collect();
    let mut value = serde_json::json!({
        "id": form.id.spelled(),
        "sessionID": form.owner.spelled(),
        "title": form.title,
        "fields": fields,
    });
    if !form.metadata.is_empty() {
        value["metadata"] = serde_json::json!(form.metadata);
    }
    value
}

/// The authoritative validation refusal retains the form's own identity.
pub fn decode_form_invalid_answer(bytes: &[u8]) -> Result<(FormWireId, String), WireError> {
    let dto: grapt_api::u3a::FormInvalidAnswerErrorDto = decode_page(bytes, limits()?)?;
    Ok((
        dto.id().get().clone(),
        dto.message().get().as_str().to_owned(),
    ))
}

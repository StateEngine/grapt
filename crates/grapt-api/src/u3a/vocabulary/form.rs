//! Retained form catalog vocabulary, with bounded field specifications.

use super::catalog::LocationInfoDto;
use super::prelude::*;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct FormFieldDto(OpenObject);

impl NamedSchema for FormFieldDto {
    const OPENAPI_NAME: &'static str = "Form.Field";
}

impl<'de> serde::Deserialize<'de> for FormFieldDto {
    fn deserialize<D: serde::Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let bounded = <OpenObject as serde::Deserialize>::deserialize(input)?;
        let value = serde_json::to_value(&bounded).map_err(serde::de::Error::custom)?;
        validate_field(&value).map_err(serde::de::Error::custom)?;
        Ok(Self(bounded))
    }
}

fn numeric(value: &serde_json::Value) -> bool {
    serde_json::from_value::<FiniteOrSentinelF64>(value.clone()).is_ok()
}

fn validate_field(value: &serde_json::Value) -> Result<(), &'static str> {
    let object = value.as_object().ok_or("field must be an object")?;
    let kind = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or("field type is required")?;
    if !value.get("key").is_some_and(serde_json::Value::is_string) {
        return Err("field key is required");
    }
    let extra: &[&str] = match kind {
        "string" => &[
            "format",
            "minLength",
            "maxLength",
            "pattern",
            "placeholder",
            "default",
            "options",
            "custom",
        ],
        "number" | "integer" => &["minimum", "maximum", "default"],
        "boolean" => &["default"],
        "multiselect" => &["options", "minItems", "maxItems", "custom", "default"],
        "external" => &["url"],
        _ => return Err("unknown form field type"),
    };
    for (key, entry) in object {
        let common = if kind == "external" {
            ["key", "type", "title", "description"].contains(&key.as_str())
        } else {
            ["key", "type", "title", "description", "required", "when"].contains(&key.as_str())
        };
        if !common && !extra.contains(&key.as_str()) {
            return Err("unknown field member");
        }
        let valid = match key.as_str() {
            "key" | "type" | "title" | "description" | "pattern" | "placeholder" | "url" => {
                entry.is_string()
            }
            "required" | "custom" => entry.is_boolean(),
            "minLength" | "maxLength" | "minItems" | "maxItems" => entry.as_u64().is_some(),
            "minimum" | "maximum" => numeric(entry),
            "format" => entry
                .as_str()
                .is_some_and(|s| ["email", "uri", "date", "date-time"].contains(&s)),
            "default" => match kind {
                "string" => entry.is_string(),
                "number" | "integer" => numeric(entry),
                "boolean" => entry.is_boolean(),
                "multiselect" => entry
                    .as_array()
                    .is_some_and(|a| a.iter().all(serde_json::Value::is_string)),
                _ => false,
            },
            "when" => entry.as_array().is_some_and(|a| {
                a.iter().all(|v| {
                    v.as_object().is_some_and(|o| o.len() == 3)
                        && v.get("key").is_some_and(serde_json::Value::is_string)
                        && v.get("op")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|s| s == "eq" || s == "neq")
                        && v.get("value")
                            .is_some_and(|v| v.is_string() || v.is_boolean() || v.is_number())
                })
            }),
            "options" => entry.as_array().is_some_and(|a| {
                a.iter().all(|v| {
                    v.as_object().is_some_and(|o| {
                        o.keys()
                            .all(|k| ["value", "label", "description"].contains(&k.as_str()))
                    }) && v.get("value").is_some_and(serde_json::Value::is_string)
                        && v.get("label").is_some_and(serde_json::Value::is_string)
                        && v.get("description")
                            .is_none_or(serde_json::Value::is_string)
                })
            }),
            _ => false,
        };
        if !valid {
            return Err("form field member has an invalid value");
        }
    }
    if kind == "external" && !value.get("url").is_some_and(serde_json::Value::is_string) {
        return Err("external field URL is required");
    }
    if kind == "multiselect"
        && !value
            .get("options")
            .is_some_and(serde_json::Value::is_array)
    {
        return Err("multiselect options are required");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct FormFieldsDto(BoundedVec<FormFieldDto, MAX_NESTED_ITEMS>);

impl<'de> serde::Deserialize<'de> for FormFieldsDto {
    fn deserialize<D: serde::Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let fields =
            <BoundedVec<FormFieldDto, MAX_NESTED_ITEMS> as serde::Deserialize>::deserialize(input)?;
        if fields.as_slice().is_empty() {
            return Err(serde::de::Error::custom(
                "a retained form requires at least one field",
            ));
        }
        Ok(Self(fields))
    }
}

closed_object! { pub struct FormInfoDto => "Form.Info" {
    required id: WireId<grapt_types::FormId> => "id",
    required session_id: DtoString => "sessionID",
    required title: DtoString => "title",
    optional metadata: OpenObject => "metadata",
    required fields: FormFieldsDto => "fields",
} }

closed_object! { pub struct FormCreatedData => "Form.Event.Created" {
    required form: FormInfoDto => "form",
    required session_id: DtoString => "sessionID",
} }
closed_object! { pub struct FormRepliedData => "Form.Event.Replied" {
    required id: WireId<grapt_types::FormId> => "id",
    required session_id: DtoString => "sessionID",
    required answer: FormAnswerDto => "answer",
} }
closed_object! { pub struct FormCancelledData => "Form.Event.Cancelled" {
    required id: WireId<grapt_types::FormId> => "id",
    required session_id: DtoString => "sessionID",
} }
closed_object! { pub struct FormSuspendedData => "Form.Event.Suspended" {
    required form_id: WireId<grapt_types::FormId> => "formID",
    required session_id: DtoString => "sessionID",
    required reason: FormSuspendedReasonDto => "reason",
} }

// The retained legacy question API has no question producer in this product.
// Its empty list is distinct from pending forms and cannot encode pretend rows.
closed_object! { pub struct QuestionListResponseDto => "inline:legacy-question-list-response" {
    required data: [(); 0] => "data",
    required location: LocationInfoDto => "location",
} }

closed_object! { pub struct FormListResponseDto => "inline:form-list-response" {
    required data: BoundedVec<FormInfoDto, MAX_CATALOG_ROWS> => "data",
    required location: LocationInfoDto => "location",
} }

closed_object! { pub struct SessionFormListResponseDto => "inline:session-form-list-response" {
    required data: BoundedVec<FormInfoDto, MAX_CATALOG_ROWS> => "data",
} }

/// One answer value. The frozen `Form.Value` union is string, number, boolean
/// or a list of strings — nothing else, and notably not a nested object, so an
/// answer cannot smuggle arbitrary structure past the field specification.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct FormValueDto(serde_json::Value);

impl FormValueDto {
    pub fn get(&self) -> &serde_json::Value {
        &self.0
    }
}

impl NamedSchema for FormValueDto {
    const OPENAPI_NAME: &'static str = "Form.Value";
}

impl<'de> serde::Deserialize<'de> for FormValueDto {
    fn deserialize<D: serde::Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        let value = <serde_json::Value as serde::Deserialize>::deserialize(input)?;
        let admitted = match &value {
            serde_json::Value::String(_) | serde_json::Value::Bool(_) => true,
            // A number must be finite and representable; the shared numeric
            // guard is the same one every other numeric field uses.
            serde_json::Value::Number(_) => {
                serde_json::from_value::<FiniteOrSentinelF64>(value.clone()).is_ok()
            }
            serde_json::Value::Array(items) => items.iter().all(serde_json::Value::is_string),
            serde_json::Value::Null | serde_json::Value::Object(_) => false,
        };
        if !admitted {
            return Err(serde::de::Error::custom(
                "a form answer value must be a string, number, boolean or string list",
            ));
        }
        Ok(Self(value))
    }
}

/// A whole answer: field key to value.
///
/// Bounded by the same ceiling a form's field specification is, because an
/// answer cannot name more fields than a form can have.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct FormAnswerDto(BoundedMap<DtoString, FormValueDto, MAX_NESTED_ITEMS>);

impl FormAnswerDto {
    pub fn get(&self) -> &BoundedMap<DtoString, FormValueDto, MAX_NESTED_ITEMS> {
        &self.0
    }
}

impl NamedSchema for FormAnswerDto {
    const OPENAPI_NAME: &'static str = "Form.Answer";
}

impl<'de> serde::Deserialize<'de> for FormAnswerDto {
    fn deserialize<D: serde::Deserializer<'de>>(input: D) -> Result<Self, D::Error> {
        <BoundedMap<DtoString, FormValueDto, MAX_NESTED_ITEMS> as serde::Deserialize>::deserialize(
            input,
        )
        .map(Self)
    }
}

string_enum! { pub enum FormPendingStatus { Pending => "pending" } }
string_enum! { pub enum FormAnsweredStatus { Answered => "answered" } }
string_enum! { pub enum FormCancelledStatus { Cancelled => "cancelled" } }
string_enum! { pub enum FormSuspendedStatus { Suspended => "suspended" } }
string_enum! { pub enum FormSuspendedReasonDto {
    Shutdown => "shutdown",
    ProcessLoss => "process-loss",
} }

closed_object! { pub struct FormStatePendingDto => "Form.State.Pending" {
    required status: FormPendingStatus => "status",
} }

closed_object! { pub struct FormStateAnsweredDto => "Form.State.Answered" {
    required status: FormAnsweredStatus => "status",
    required answer: FormAnswerDto => "answer",
} }

closed_object! { pub struct FormStateCancelledDto => "Form.State.Cancelled" {
    required status: FormCancelledStatus => "status",
} }

// Suspended carries its own reason and is a DISTINCT member from cancelled, so
// a waiter can never mistake a shutdown or a lost process for a genuine user
// cancel. That distinction is the frozen schema's, not this projection's.
closed_object! { pub struct FormStateSuspendedDto => "Form.State.Suspended" {
    required status: FormSuspendedStatus => "status",
    required reason: FormSuspendedReasonDto => "reason",
} }

tagged_union! {
    pub enum FormStateDto => "Form.State", tag "status" {
        Pending(FormStatePendingDto) => "pending",
        Answered(FormStateAnsweredDto) => "answered",
        Cancelled(FormStateCancelledDto) => "cancelled",
        Suspended(FormStateSuspendedDto) => "suspended",
    }
}

closed_object! { pub struct FormCreateRequestDto => "Form.CreatePayload" {
    optional id: WireId<grapt_types::FormId> => "id",
    required title: DtoString => "title",
    optional metadata: OpenObject => "metadata",
    required fields: FormFieldsDto => "fields",
} }

closed_object! { pub struct FormReplyRequestDto => "Form.Reply" {
    required answer: FormAnswerDto => "answer",
} }

closed_object! { pub struct FormResponseDto => "inline:form-response" {
    required data: FormInfoDto => "data",
} }

closed_object! { pub struct FormStateResponseDto => "inline:form-state-response" {
    required data: FormStateDto => "data",
} }

// The three form refusals are separate members because a client acts on each
// one differently: absent is not settled, and settled is not invalid. They keep
// the frozen statuses — 404, 409, 400 — so a caller can tell "there is no such
// form" from "you are too late" from "that answer does not fit the fields".

string_enum! {
    pub enum FormNotFoundErrorDtoTag {
        FormNotFoundError => "FormNotFoundError",
    }
}

closed_object! {
    pub struct FormNotFoundErrorDto => "FormNotFoundError" {
        required tag: FormNotFoundErrorDtoTag => "_tag",
        required id: WireId<grapt_types::FormId> => "id",
        required message: DtoString => "message",
    }
}

string_enum! {
    pub enum FormAlreadySettledErrorDtoTag {
        FormAlreadySettledError => "FormAlreadySettledError",
    }
}

closed_object! {
    pub struct FormAlreadySettledErrorDto => "FormAlreadySettledError" {
        required tag: FormAlreadySettledErrorDtoTag => "_tag",
        required id: WireId<grapt_types::FormId> => "id",
        required message: DtoString => "message",
    }
}

// The message is the validator's own prose and is the ONLY place the rejected
// field appears — the frozen error carries no structured field key, so this
// projection does not invent one.
string_enum! {
    pub enum FormInvalidAnswerErrorDtoTag {
        FormInvalidAnswerError => "FormInvalidAnswerError",
    }
}

closed_object! {
    pub struct FormInvalidAnswerErrorDto => "FormInvalidAnswerError" {
        required tag: FormInvalidAnswerErrorDtoTag => "_tag",
        required id: WireId<grapt_types::FormId> => "id",
        required message: DtoString => "message",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_fields_preserve_the_frozen_nonfinite_sentinels() {
        for kind in ["number", "integer"] {
            for key in ["minimum", "maximum", "default"] {
                for number in [
                    serde_json::json!(1.5),
                    serde_json::json!("NaN"),
                    serde_json::json!("Infinity"),
                    serde_json::json!("-Infinity"),
                ] {
                    let mut value = serde_json::json!({"key":"value", "type":kind});
                    value[key] = number;
                    let decoded = serde_json::from_value::<FormFieldDto>(value.clone())
                        .unwrap_or_else(|error| panic!("legal numeric field refused: {error}"));
                    assert_eq!(serde_json::to_value(decoded).ok(), Some(value));
                }
                for number in [
                    serde_json::json!("1.5"),
                    serde_json::json!("nan"),
                    serde_json::Value::Null,
                ] {
                    let mut value = serde_json::json!({"key":"value", "type":kind});
                    value[key] = number;
                    assert!(serde_json::from_value::<FormFieldDto>(value).is_err());
                }
            }
        }
    }

    #[test]
    fn fields_accept_tag_order_but_reject_foreign_null_duplicate_and_empty_shapes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let field =
            r#"{"key":"choice","options":[{"value":"one","label":"One"}],"type":"multiselect"}"#;
        let decoded: FormFieldDto = serde_json::from_str(field)?;
        assert_eq!(
            serde_json::to_value(decoded)?,
            serde_json::from_str::<serde_json::Value>(field)?
        );
        for invalid in [
            r#"{"key":"x","type":"boolean","pattern":"x"}"#,
            r#"{"key":"x","type":"string","default":null}"#,
            r#"{"key":"x","type":"boolean","key":"y"}"#,
            r#"{"key":"x","type":"multiselect"}"#,
            r#"{"key":"x","type":"external","url":null}"#,
        ] {
            assert!(
                serde_json::from_str::<FormFieldDto>(invalid).is_err(),
                "accepted {invalid}"
            );
        }
        assert!(serde_json::from_str::<FormFieldsDto>("[]").is_err());
        Ok(())
    }
}

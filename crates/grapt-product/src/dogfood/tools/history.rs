use serde::Deserialize;

use super::*;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::dogfood) struct HistoryArguments {
    pub(in crate::dogfood) call: Option<NonEmptyString>,
    pub(in crate::dogfood) cursor: Option<u64>,
    pub(in crate::dogfood) offset: Option<u64>,
}

impl ToolArguments for HistoryArguments {
    const TOOL_NAME: &'static str = HISTORY_TOOL;

    fn provider_schema() -> String {
        // One flat object. Anthropic refuses an input schema with oneOf,
        // anyOf or allOf at its top level, and it is the whole request that
        // fails. Responses strict mode requires every published property, so
        // each field must accept null; null means omitted, never an invented
        // ID. A call selects output retrieval; cursor only pages listings.
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": [],
            "properties": {
                "call": { "type": ["string", "null"], "minLength": 1,
                    "description": "Listed ID. Ignores cursor. Null when paging listings." },
                "cursor": { "type": ["integer", "null"], "minimum": 0,
                    "description": "Ignored when call is present. Null when fetching a call." },
                "offset": { "type": ["integer", "null"], "minimum": 0,
                    "description": "Byte offset. Requires call. Null when listing." }
            },
        })
        .to_string()
    }
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: HistoryArguments = decode_input(input)?;
    if args.call.is_none() && args.offset.is_some() {
        return Err("history offset requires a call identity".to_owned());
    }
    // Models may fill every published field. A call unambiguously selects
    // output retrieval; the listing cursor has no meaning in that mode.
    let cursor = args.call.is_none().then_some(args.cursor).flatten();
    let call = args
        .call
        .map(NonEmptyString::into_inner)
        .map(|raw| {
            ToolCallId::new(raw).ok_or_else(|| "history call identity is invalid".to_owned())
        })
        .transpose()?;
    let resource = call.as_ref().map_or_else(
        || format!("{} from cursor {}", HISTORY_TOOL, cursor.unwrap_or(0)),
        |call| format!("{} output at byte {}", call, args.offset.unwrap_or(0)),
    );
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        target: EffectTarget::History {
            call,
            cursor,
            offset: args.offset,
        },
        action: HISTORY_TOOL.to_owned(),
        resource,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_mode_null_fill_lists_from_the_first_page() -> Result<(), String> {
        let mut schema: serde_json::Value =
            serde_json::from_str(&HistoryArguments::provider_schema())
                .map_err(|error| error.to_string())?;
        for keyword in ["anyOf", "oneOf", "allOf"] {
            assert!(schema.get(keyword).is_none());
        }
        schema["required"] = serde_json::json!(["call", "cursor", "offset"]);
        let value = serde_json::json!({"call":null,"cursor":null,"offset":null});
        schema::validate(
            &schema.to_string(),
            &serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )?;
        let input = BoundedToolInput::new(value.to_string().into_bytes())
            .map_err(|error| error.to_string())?;
        let prepared = prepare(Path::new("."), &input)?;
        assert!(
            matches!(
                prepared.target,
                EffectTarget::History {
                    call: None,
                    cursor: None,
                    offset: None
                }
            ),
            "{value}"
        );
        Ok(())
    }
}

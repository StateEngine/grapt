//! Read the caller's public conversation through the existing application routes.
use super::*;
use grapt_api::u3a::WireId;

pub(super) const TOOL: &str = "read_history";
pub(super) const DESCRIPTION: &str =
    "Messages: {} lists; cursor pages; message reads JSON at offset=nextOffset.";
const CHUNK_BYTES: usize = 8 * 1024;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Arguments {
    pub(super) message: Option<NonEmptyString>,
    pub(super) cursor: Option<NonEmptyString>,
    pub(super) offset: Option<u64>,
}

impl ToolArguments for Arguments {
    const TOOL_NAME: &'static str = TOOL;

    fn provider_schema() -> String {
        // Responses strict mode requires every published property, so each
        // field must accept null; null means omitted, never an empty ID.
        serde_json::json!({"type":"object","additionalProperties":false,
            "properties":{
                "message":{"type":["string","null"],"minLength":1,"description":"Message ID to fetch; null when listing."},
                "cursor":{"type":["string","null"],"minLength":1,"description":"Listing page token; null when fetching a message."},
                "offset":{"type":["integer","null"],"minimum":0,"description":"Byte offset inside the message; null when listing."}
            },
            "required":[]}).to_string()
    }
}

#[derive(Debug, Clone)]
pub(in crate::dogfood) enum Request {
    List { cursor: Option<String> },
    Message { id: MessageId, offset: usize },
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: Arguments = decode_input(input)?;
    let request = match args.message {
        Some(message) => {
            if args.cursor.is_some() {
                return Err("read_history message and cursor are mutually exclusive".into());
            }
            let id = WireId::<MessageId>::parse(message.into_inner())
                .ok_or("invalid history message identity")?
                .into_inner();
            let offset = usize::try_from(args.offset.unwrap_or(0))
                .map_err(|_| "history offset is too large")?;
            Request::Message { id, offset }
        }
        None => {
            if args.offset.is_some() {
                return Err("read_history offset requires message".into());
            }
            let cursor = args.cursor.map(NonEmptyString::into_inner);
            if cursor.as_ref().is_some_and(|cursor| cursor.len() > 1024) {
                return Err("history cursor exceeds its bound".into());
            }
            Request::List { cursor }
        }
    };
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        target: EffectTarget::ReadHistory(request),
        // Conversation recovery obeys the existing session-history permission,
        // including an explicit configured denial of history.
        action: HISTORY_TOOL.into(),
        resource: "this session's public conversation".into(),
    })
}

pub(super) fn run(
    seam: &crate::dogfood::subagent::ChildSeam,
    session: &SessionId,
    request: Request,
) -> Result<Vec<u8>, String> {
    let app = seam
        .get()
        .and_then(std::sync::Weak::upgrade)
        .ok_or("the history application is unavailable")?;
    read(&app, session, request)
}

pub(in crate::dogfood) fn read(
    app: &crate::dogfood::DogfoodApplication,
    session: &SessionId,
    request: Request,
) -> Result<Vec<u8>, String> {
    // The model cannot supply a Session or principal. The authenticated,
    // permission-governed runner supplies its own Session; use the same
    // bounded, redacted handlers as the public message routes.
    let limits = grapt_api::u3a::U3aLimits::production().map_err(|error| error.to_string())?;
    let result = match request {
        Request::List { cursor } => {
            let query = crate::dogfood::decode(serde_json::json!({"limit":"16", "cursor":cursor}))?;
            let page = crate::dogfood::history::messages_get(
                app,
                session,
                query,
                grapt_api::u3a::MessageListView::Durable,
            )
            .map_err(|error| format!("history listing refused: {error:?}"))?;
            // Keep the route's byte bound even though only its public identities
            // are returned here. Content is fetched on demand, never dumped whole.
            let encoded =
                grapt_api::u3a::encode_page(&page, limits).map_err(|error| error.to_string())?;
            let page: serde_json::Value =
                serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
            let messages = page["data"].as_array().ok_or("history page has no data")?
                .iter().map(|row| serde_json::json!({"id":row["id"], "type":row["type"], "time":row["time"]}))
                .collect::<Vec<_>>();
            serde_json::json!({"messages":messages, "next_cursor":page["cursor"]["next"]})
        }
        Request::Message { id, offset } => {
            let message = crate::dogfood::history::message_get(app, session, &id)
                .map_err(|error| format!("history message refused: {error:?}"))?;
            let bytes =
                grapt_api::u3a::encode_page(&message, limits).map_err(|error| error.to_string())?;
            let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
            if !text.is_char_boundary(offset) {
                return Err("history offset is outside the message or splits UTF-8".into());
            }
            let mut end = offset.saturating_add(CHUNK_BYTES).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            serde_json::json!({"text":&text[offset..end], "offset":offset,
                "totalBytes":text.len(), "nextOffset":(end < text.len()).then_some(end)})
        }
    };
    serde_json::to_vec(&result).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_mode_null_fill_reaches_the_single_message_fetch() -> Result<(), String> {
        let mut schema: serde_json::Value = serde_json::from_str(&Arguments::provider_schema())
            .map_err(|error| error.to_string())?;
        for keyword in ["anyOf", "oneOf", "allOf"] {
            assert!(schema.get(keyword).is_none());
        }
        // The hosted Responses route forces every published property; null
        // must mean omitted, never an invented ID or a refused pair.
        schema["required"] = serde_json::json!(["message", "cursor", "offset"]);
        for (value, fetches) in [
            (
                serde_json::json!({"message":null,"cursor":null,"offset":null}),
                false,
            ),
            (
                serde_json::json!({"message":"msg_1","cursor":null,"offset":null}),
                true,
            ),
        ] {
            schema::validate(
                &schema.to_string(),
                &serde_json::to_vec(&value).map_err(|error| error.to_string())?,
            )?;
            let input = BoundedToolInput::new(value.to_string().into_bytes())
                .map_err(|error| error.to_string())?;
            let prepared = prepare(Path::new("."), &input)?;
            assert_eq!(
                matches!(
                    prepared.target,
                    EffectTarget::ReadHistory(Request::Message { .. })
                ),
                fetches,
                "{value}"
            );
        }
        let pair = serde_json::json!({"message":"msg_1","cursor":"c","offset":null});
        let input = BoundedToolInput::new(pair.to_string().into_bytes())
            .map_err(|error| error.to_string())?;
        assert_eq!(
            prepare(Path::new("."), &input).err(),
            Some("read_history message and cursor are mutually exclusive".to_owned())
        );
        Ok(())
    }
    #[test]
    fn arguments_cannot_claim_a_session_or_mix_fetch_and_list() -> Result<(), String> {
        for raw in [
            r#"{"session":"ses_foreign"}"#,
            r#"{"offset":1}"#,
            r#"{"message":"bad"}"#,
            r#"{"message":"msg_foreign","cursor":"x"}"#,
        ] {
            let input =
                BoundedToolInput::new(raw.as_bytes().to_vec()).map_err(|e| e.to_string())?;
            assert!(prepare(Path::new("/tmp"), &input).is_err());
        }
        Ok(())
    }
    #[test]
    fn reader_uses_inspection_permission_and_obeys_history_denials() -> Result<(), String> {
        use grapt_permission::preset::{decide, Compose, Effect, Layer, LayerName, Preset, Rule};
        let input = BoundedToolInput::new(b"{}".to_vec()).map_err(|error| error.to_string())?;
        let prepared = prepare(Path::new("/tmp"), &input)?;
        let mut layers = vec![Layer::preset(Preset::Inspect)];
        assert_eq!(
            decide(&prepared.action, &[&prepared.resource], &layers).effect,
            Effect::Allow
        );
        layers.push(Layer {
            name: LayerName::Config,
            rules: vec![Rule::new("history", "*", Effect::Deny)],
            floor: false,
            compose: Compose::Narrow,
        });
        assert_eq!(
            decide(&prepared.action, &[&prepared.resource], &layers).effect,
            Effect::Deny
        );
        Ok(())
    }
}

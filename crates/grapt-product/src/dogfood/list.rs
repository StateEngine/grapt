//! Retained session-list query and opaque cursor adapter over the existing owner.

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use grapt_api::u3a::{
    OpenObject, SessionListApplicationError, SessionListQueryDto, SessionsResponseDto,
};
use grapt_runtime::SessionPorts;
use grapt_session::{
    BoundedSearch, NonNegativeMs, ParentFilter, SessionListCriteria, SessionListDirection,
    SessionListKey, SessionListLimits, SessionListOrder, SessionListScope,
};
use grapt_types::{DeterministicAuthorities, LocationRef, ProjectId, SessionId, WorkspaceId};
use serde_json::{json, Value};
use std::sync::Arc;

use super::support::{decode, invalid};
use super::DogfoodApplication;

const MAX_CURSOR_BYTES: usize = 16_384;
const DEFAULT_ROWS: usize = 50;

struct PageRequest {
    query: Value,
    criteria: SessionListCriteria,
}

fn request_error(error: impl ToString) -> SessionListApplicationError {
    SessionListApplicationError::InvalidRequest(invalid(error))
}

fn cursor_error() -> SessionListApplicationError {
    match decode(json!({"_tag":"InvalidCursorError", "message":"Invalid cursor"})) {
        Ok(error) => SessionListApplicationError::InvalidCursor(error),
        Err(error) => request_error(error),
    }
}

impl PageRequest {
    fn parse(input: SessionListQueryDto) -> Result<Self, SessionListApplicationError> {
        let mut input = serde_json::to_value(input).map_err(request_error)?;
        // Query text spells a root filter as "null"; cursor JSON carries null.
        if text(&input, "parentID") == Some("null") {
            input["parentID"] = Value::Null;
        }
        let rows = match input.get("limit").filter(|value| !value.is_null()) {
            Some(value) => value
                .as_str()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| request_error("limit must be a positive integer"))?,
            None => DEFAULT_ROWS,
        };
        let cursor = input.get("cursor").filter(|value| !value.is_null());
        let has_cursor = cursor.is_some();
        let query = match cursor {
            Some(raw) => {
                let raw = raw.as_str().ok_or_else(cursor_error)?;
                if raw.len() > MAX_CURSOR_BYTES {
                    return Err(cursor_error());
                }
                let bytes = URL_SAFE_NO_PAD
                    .decode(raw)
                    .or_else(|_| URL_SAFE.decode(raw))
                    .map_err(|_| cursor_error())?;
                // OpenObject enforces duplicate-member, scalar, depth and node bounds
                // before any conversion into an ordinary JSON value.
                let bounded: OpenObject =
                    serde_json::from_slice(&bytes).map_err(|_| cursor_error())?;
                let query = serde_json::to_value(bounded).map_err(|_| cursor_error())?;
                validate_cursor(&query).map_err(|_| cursor_error())?;
                query
            }
            None => input,
        };
        let criteria = criteria(&query, rows).map_err(|error| {
            if has_cursor {
                cursor_error()
            } else {
                request_error(error)
            }
        })?;
        // Only the effective query travels in a cursor. The new request's limit
        // remains independent, as in the retained HTTP handler.
        let mut query = query;
        let object = query
            .as_object_mut()
            .ok_or_else(|| request_error("query is not an object"))?;
        object.remove("limit");
        object.remove("cursor");
        object.remove("anchor");
        object.retain(|key, value| key == "parentID" || !value.is_null());
        if object.contains_key("directory") {
            object.remove("project");
            object.remove("subpath");
        }
        Ok(Self { query, criteria })
    }

    fn cursor(&self, key: &SessionListKey, direction: &str) -> Result<String, String> {
        let mut value = self.query.clone();
        value["anchor"] = json!({"id":key.session_id().as_str(),
            "time":key.updated_ms().value(), "direction":direction});
        let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        let encoded = URL_SAFE_NO_PAD.encode(bytes);
        if encoded.len() > MAX_CURSOR_BYTES {
            return Err("session cursor exceeds its byte bound".to_owned());
        }
        Ok(encoded)
    }
}

fn text<'a>(query: &'a Value, key: &str) -> Option<&'a str> {
    query.get(key).and_then(Value::as_str)
}

fn validate_cursor(query: &Value) -> Result<(), String> {
    let object = query.as_object().ok_or("cursor must be an object")?;
    if object.keys().any(|key| {
        ![
            "workspace",
            "order",
            "search",
            "parentID",
            "directory",
            "project",
            "subpath",
            "anchor",
        ]
        .contains(&key.as_str())
    }) {
        return Err("unknown cursor member".to_owned());
    }
    let anchor = query
        .get("anchor")
        .and_then(Value::as_object)
        .ok_or("cursor anchor absent")?;
    if anchor.len() != 3
        || !anchor.contains_key("id")
        || !anchor.contains_key("time")
        || !anchor.contains_key("direction")
    {
        return Err("cursor anchor malformed".to_owned());
    }
    let mut adapted = query.clone();
    adapted
        .as_object_mut()
        .ok_or("cursor malformed")?
        .remove("anchor");
    // Reuse the same closed DTO field validation as a first-page request.
    decode::<SessionListQueryDto>(adapted)?;
    Ok(())
}

fn criteria(query: &Value, rows: usize) -> Result<SessionListCriteria, String> {
    let workspace = text(query, "workspace")
        .map(|raw| WorkspaceId::new(raw).ok_or_else(|| "workspace identity invalid".to_owned()))
        .transpose()?;
    let scope = if let Some(directory) = text(query, "directory") {
        SessionListScope::Directory(
            LocationRef::new(directory, workspace.clone()).map_err(|e| e.to_string())?,
        )
    } else if let Some(project) = text(query, "project") {
        SessionListScope::project(
            ProjectId::new(project).ok_or("project identity invalid")?,
            text(query, "subpath"),
        )
    } else {
        SessionListScope::All
    };
    let parent = match query.get("parentID") {
        None => ParentFilter::Any,
        Some(Value::Null) => ParentFilter::Root,
        Some(value) => ParentFilter::Child(
            value
                .as_str()
                .ok_or("parent identity invalid")?
                .parse::<SessionId>()
                .map_err(|e| e.to_string())?,
        ),
    };
    let order = match text(query, "order") {
        None | Some("desc") => SessionListOrder::Descending,
        Some("asc") => SessionListOrder::Ascending,
        Some(_) => return Err("session order invalid".to_owned()),
    };
    let (direction, anchor) = match query.get("anchor") {
        None => (SessionListDirection::Next, None),
        Some(anchor) => {
            let direction = match text(anchor, "direction") {
                Some("next") => SessionListDirection::Next,
                Some("previous") => SessionListDirection::Previous,
                _ => return Err("cursor direction invalid".to_owned()),
            };
            let time = anchor
                .get("time")
                .and_then(Value::as_i64)
                .and_then(NonNegativeMs::new)
                .ok_or("cursor time invalid")?;
            let id = text(anchor, "id")
                .ok_or("cursor identity absent")?
                .parse::<SessionId>()
                .map_err(|e| e.to_string())?;
            (direction, Some(SessionListKey::new(time, id)))
        }
    };
    let limits = SessionListLimits::production();
    let search = text(query, "search")
        .map(|value| BoundedSearch::new(value, limits))
        .transpose()
        .map_err(|e| e.to_string())?;
    SessionListCriteria::new(
        scope, workspace, search, parent, order, direction, anchor, rows, limits,
    )
    .map_err(|e| e.to_string())
}

pub(super) fn response(
    application: &DogfoodApplication,
    query: SessionListQueryDto,
) -> Result<SessionsResponseDto, SessionListApplicationError> {
    let request = PageRequest::parse(query)?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let ports = SessionPorts::new(Arc::clone(&application.inbox), &authorities);
    let page = ports
        .list(&request.criteria)
        .map_err(|_| SessionListApplicationError::Unavailable)?;
    let mut data = Vec::with_capacity(page.items().len());
    for item in page.items() {
        data.push(
            super::list_metadata::row(&ports, item)
                .map_err(|_| SessionListApplicationError::Unavailable)?,
        );
    }
    let mut cursor = json!({});
    if let Some(first) = page.items().first() {
        cursor["previous"] = json!(request
            .cursor(&first.key, "previous")
            .map_err(request_error)?);
    }
    if let Some(last) = page.items().last() {
        cursor["next"] = json!(request.cursor(&last.key, "next").map_err(request_error)?);
    }
    decode(json!({"data":data,"cursor":cursor}))
        .map_err(|_| SessionListApplicationError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(value: Value) -> Result<PageRequest, SessionListApplicationError> {
        PageRequest::parse(decode(value).map_err(request_error)?)
    }

    #[test]
    fn cursor_preserves_effective_scope_order_parent_and_anchor() {
        let request = request(json!({"project":"project-a", "parentID":null,
            "order":"asc", "search":"retained", "limit":"1"}))
        .unwrap_or_else(|error| panic!("first-page request: {error:?}"));
        let key = SessionListKey::new(
            NonNegativeMs::new(17).unwrap_or_else(|| panic!("time")),
            "ses_list_first"
                .parse::<SessionId>()
                .unwrap_or_else(|error| panic!("id: {error}")),
        );
        let encoded = request
            .cursor(&key, "previous")
            .unwrap_or_else(|error| panic!("cursor: {error}"));
        let decoded: Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(&encoded)
                .unwrap_or_else(|error| panic!("base64: {error}")),
        )
        .unwrap_or_else(|error| panic!("json: {error}"));
        assert_eq!(
            decoded,
            json!({"project":"project-a", "parentID":null, "order":"asc",
            "search":"retained", "anchor":{"id":"ses_list_first","time":17,"direction":"previous"}})
        );
        let resumed = PageRequest::parse(
            decode(json!({"cursor":encoded,"project":"ignored-outer", "limit":"2"}))
                .unwrap_or_else(|error| panic!("resume dto: {error}")),
        )
        .unwrap_or_else(|error| panic!("resume: {error:?}"));
        assert_eq!(resumed.criteria.scope, request.criteria.scope);
        assert_eq!(resumed.criteria.parent, ParentFilter::Root);
        assert_eq!(resumed.criteria.order, SessionListOrder::Ascending);
        assert_eq!(resumed.criteria.direction, SessionListDirection::Previous);
        assert_eq!(resumed.criteria.anchor, Some(key));
    }

    #[test]
    fn cursor_corruption_never_restarts_an_unfiltered_first_page() {
        for raw in [
            r#"{}"#,
            r#"{"anchor":{"id":"ses_a","time":1,"direction":"next","id":"ses_b"}}"#,
            r#"{"anchor":{"id":"ses_a","time":-1,"direction":"next"}}"#,
            r#"{"anchor":{"id":"ses_a","time":1,"direction":"sideways"}}"#,
            r#"{"anchor":{"id":"ses_a","time":1,"direction":"next"},"foreign":true}"#,
        ] {
            assert!(
                matches!(
                    request(json!({"cursor":URL_SAFE_NO_PAD.encode(raw)})),
                    Err(SessionListApplicationError::InvalidCursor(_))
                ),
                "accepted {raw}"
            );
        }
        assert!(matches!(
            request(json!({"cursor":"%%%"})),
            Err(SessionListApplicationError::InvalidCursor(_))
        ));
        for limit in ["0", "129", "bad"] {
            assert!(request(json!({"limit":limit})).is_err());
        }
    }
}

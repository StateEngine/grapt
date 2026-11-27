//! Bounded query decoding for the Dogfood-0 route set.

use std::collections::BTreeMap;

use grapt_schema::{Optional, OptionalNullable, TriStateEvidence};

use super::codec::WireIdentity;
use super::*;

pub const MAX_QUERY_BYTES: usize = 8_192;
pub const MAX_QUERY_PAIRS: usize = 32;

pub(super) fn saved_permission_project(
    raw: Option<&str>,
) -> Result<Option<WireId<grapt_types::ProjectId>>, QueryError> {
    let mut pairs = QueryPairs::parse(raw)?;
    let project = pairs
        .take("projectID")
        .map(|value| wire_id(value, "projectID"))
        .transpose()?;
    pairs.finish()?;
    Ok(project)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryError {
    TooLarge,
    TooManyPairs,
    InvalidEncoding,
    DuplicateField,
    UnknownField,
    InvalidValue { field: &'static str },
    ConflictingLocationForms,
}

struct QueryPairs {
    values: BTreeMap<String, String>,
}

impl QueryPairs {
    fn parse(raw: Option<&str>) -> Result<Self, QueryError> {
        let raw = raw.unwrap_or_default();
        if raw.len() > MAX_QUERY_BYTES {
            return Err(QueryError::TooLarge);
        }
        let mut values = BTreeMap::new();
        if raw.is_empty() {
            return Ok(Self { values });
        }
        for (index, pair) in raw.split('&').enumerate() {
            if index >= MAX_QUERY_PAIRS {
                return Err(QueryError::TooManyPairs);
            }
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = percent_decode(key)?;
            let value = percent_decode(value)?;
            if key.len() > 64 || value.len() > MAX_QUERY_BYTES {
                return Err(QueryError::TooLarge);
            }
            if values.insert(key, value).is_some() {
                return Err(QueryError::DuplicateField);
            }
        }
        Ok(Self { values })
    }

    fn take(&mut self, key: &str) -> Option<String> {
        self.values.remove(key)
    }

    fn finish(self) -> Result<(), QueryError> {
        if self.values.is_empty() {
            Ok(())
        } else {
            Err(QueryError::UnknownField)
        }
    }
}

pub(crate) fn percent_decode(value: &str) -> Result<String, QueryError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' => {
                let Some(high) = bytes.get(index + 1).and_then(|byte| hex(*byte)) else {
                    return Err(QueryError::InvalidEncoding);
                };
                let Some(low) = bytes.get(index + 2).and_then(|byte| hex(*byte)) else {
                    return Err(QueryError::InvalidEncoding);
                };
                decoded.push((high << 4) | low);
                index += 3;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).map_err(|_| QueryError::InvalidEncoding)
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn dto_string(value: String, field: &'static str) -> Result<DtoString, QueryError> {
    DtoString::try_new(value).map_err(|_| QueryError::InvalidValue { field })
}

fn bounded_string<const MAX: usize>(
    value: String,
    field: &'static str,
) -> Result<BoundedString<MAX>, QueryError> {
    BoundedString::try_new(value).map_err(|_| QueryError::InvalidValue { field })
}

fn wire_id<T: WireIdentity>(value: String, field: &'static str) -> Result<WireId<T>, QueryError> {
    WireId::parse(value).ok_or(QueryError::InvalidValue { field })
}

fn tri<T, E, F>(
    value: Option<String>,
    field: &'static str,
    parse: F,
) -> Result<OptionalNullable<T, E>, QueryError>
where
    E: TriStateEvidence,
    F: FnOnce(String, &'static str) -> Result<T, QueryError>,
{
    match value {
        None => Ok(OptionalNullable::absent()),
        Some(value) => parse(value, field).map(OptionalNullable::value),
    }
}

fn session_workspace(
    value: Option<String>,
) -> Result<
    OptionalNullable<WorkspaceIdDto, grapt_schema::tri_state_ledger::U3aSessionListWorkspace>,
    QueryError,
> {
    match value {
        None => Ok(OptionalNullable::absent()),
        Some(value) if value == "null" => Ok(OptionalNullable::null()),
        Some(value) => WorkspaceIdDto::try_new(value)
            .map(OptionalNullable::value)
            .map_err(|_| QueryError::InvalidValue { field: "workspace" }),
    }
}

/// The event route owns no location application call yet, but it still models
/// and validates the frozen optional deep-object query rather than rejecting
/// every location-qualified subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EventLocationQuery {
    Absent,
    Null,
    Value {
        directory: Option<DtoString>,
        workspace: Option<WireId<grapt_types::WorkspaceId>>,
    },
}

/// The event route's location-only view of one subscription, kept for callers
/// that do not name a Session.
#[cfg(test)]
pub(super) fn event_location(raw: Option<&str>) -> Result<EventLocationQuery, QueryError> {
    event_query(raw).map(|query| query.location)
}

/// One `/api/event` subscription: where its frames come from, and which Session
/// it is a window onto.
///
/// The Session is what lets the service decide "the last window is gone" from
/// its own subscriber registry, rather than trusting a window that has already
/// crashed to announce itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EventQuery {
    pub(super) location: EventLocationQuery,
    pub(super) session: Option<WireId<grapt_types::SessionId>>,
    /// `retain=a,b`: Sessions this window showed before and still claims.
    /// Accepted only beside `positions=1`.
    pub(super) retained: Vec<grapt_types::SessionId>,
    /// `positions=1`: the opt-in positions subscriber class.
    pub(super) positions: bool,
}

pub(super) fn event_query(raw: Option<&str>) -> Result<EventQuery, QueryError> {
    let mut pairs = QueryPairs::parse(raw)?;
    let session = pairs
        .take("session")
        .map(|value| wire_id(value, "session"))
        .transpose()?;
    let retained = pairs
        .take("retain")
        .map(|value| retained_sessions(&value))
        .transpose()?
        .unwrap_or_default();
    let whole = pairs.take("location");
    let directory = pairs.take("location[directory]");
    let workspace = pairs.take("location[workspace]");
    let has_nested = directory.is_some() || workspace.is_some();
    let location = match (whole, has_nested) {
        (None, false) => EventLocationQuery::Absent,
        (Some(value), false) if value == "null" => EventLocationQuery::Null,
        (Some(_), false) => return Err(QueryError::InvalidValue { field: "location" }),
        (None, true) => EventLocationQuery::Value {
            directory: directory
                .map(|value| dto_string(value, "location[directory]"))
                .transpose()?,
            workspace: workspace
                .map(|value| wire_id(value, "location[workspace]"))
                .transpose()?,
        },
        (Some(_), true) => return Err(QueryError::ConflictingLocationForms),
    };
    let positions = match pairs.take("positions").as_deref() {
        None => false,
        Some("1") => true,
        Some(_) => return Err(QueryError::InvalidValue { field: "positions" }),
    };
    // A claim on a Session the window left is only meaningful to a client
    // that resumes by position; any other subscriber must not hold one.
    if !retained.is_empty() && !positions {
        return Err(QueryError::InvalidValue { field: "retain" });
    }
    pairs.finish()?;
    Ok(EventQuery {
        location,
        session,
        retained,
        positions,
    })
}

/// A comma-separated, bounded list of distinct Session ids.
fn retained_sessions(raw: &str) -> Result<Vec<grapt_types::SessionId>, QueryError> {
    let invalid = QueryError::InvalidValue { field: "retain" };
    let mut sessions = Vec::new();
    for part in raw.split(',') {
        let session: grapt_types::SessionId = part.parse().map_err(|_| invalid)?;
        if sessions.contains(&session)
            || sessions.len() == grapt_window_lease::MAX_RETAINED_SESSIONS
        {
            return Err(invalid);
        }
        sessions.push(session);
    }
    Ok(sessions)
}

fn page_order(value: String, field: &'static str) -> Result<PageOrderDto, QueryError> {
    match value.as_str() {
        "asc" => Ok(PageOrderDto::Asc),
        "desc" => Ok(PageOrderDto::Desc),
        _ => Err(QueryError::InvalidValue { field }),
    }
}

macro_rules! location_query {
    ($function:ident, $outer:ty, $inner:ty, $outer_evidence:ty, $directory_evidence:ty, $workspace_evidence:ty) => {
        pub fn $function(raw: Option<&str>) -> Result<$outer, QueryError> {
            let mut pairs = QueryPairs::parse(raw)?;
            let whole = pairs.take("location");
            let directory = pairs.take("location[directory]");
            let workspace = pairs.take("location[workspace]");
            let has_nested = directory.is_some() || workspace.is_some();
            if whole.is_some() && has_nested {
                return Err(QueryError::ConflictingLocationForms);
            }
            let location = match (whole, has_nested) {
                (None, false) => OptionalNullable::<$inner, $outer_evidence>::absent(),
                (Some(value), false) if value == "null" => {
                    OptionalNullable::<$inner, $outer_evidence>::null()
                }
                (Some(_), false) => return Err(QueryError::InvalidValue { field: "location" }),
                (None, true) => OptionalNullable::<$inner, $outer_evidence>::value(<$inner>::new(
                    tri::<DtoString, $directory_evidence, _>(
                        directory,
                        "location[directory]",
                        dto_string,
                    )?,
                    tri::<WireId<grapt_types::WorkspaceId>, $workspace_evidence, _>(
                        workspace,
                        "location[workspace]",
                        wire_id,
                    )?,
                )),
                (Some(_), true) => return Err(QueryError::ConflictingLocationForms),
            };
            let session = pairs
                .take("session")
                .map(|value| wire_id(value, "session"))
                .transpose()?;
            pairs.finish()?;
            Ok(<$outer>::new(
                location,
                session.map_or_else(Optional::absent, Optional::present),
            ))
        }
    };
}

location_query!(
    project_current,
    ProjectCurrentLocationQueryDto,
    ProjectCurrentLocationQueryValueDto,
    grapt_schema::tri_state_ledger::U3aProjectCurrentLocation,
    grapt_schema::tri_state_ledger::U3aProjectCurrentLocationDirectory,
    grapt_schema::tri_state_ledger::U3aProjectCurrentLocationWorkspace
);
location_query!(
    mcp_catalog,
    ProjectCurrentLocationQueryDto,
    ProjectCurrentLocationQueryValueDto,
    grapt_schema::tri_state_ledger::U3aProjectCurrentLocation,
    grapt_schema::tri_state_ledger::U3aProjectCurrentLocationDirectory,
    grapt_schema::tri_state_ledger::U3aProjectCurrentLocationWorkspace
);
location_query!(
    provider_list,
    ProviderLocationQueryDto,
    ProviderLocationQueryValueDto,
    grapt_schema::tri_state_ledger::U3aProviderLocation,
    grapt_schema::tri_state_ledger::U3aProviderLocationDirectory,
    grapt_schema::tri_state_ledger::U3aProviderLocationWorkspace
);
location_query!(
    model_list,
    ModelLocationQueryDto,
    ModelLocationQueryValueDto,
    grapt_schema::tri_state_ledger::U3aModelLocation,
    grapt_schema::tri_state_ledger::U3aModelLocationDirectory,
    grapt_schema::tri_state_ledger::U3aModelLocationWorkspace
);
location_query!(
    model_default,
    ModelDefaultLocationQueryDto,
    ModelDefaultLocationQueryValueDto,
    grapt_schema::tri_state_ledger::U3aModelDefaultLocation,
    grapt_schema::tri_state_ledger::U3aModelDefaultLocationDirectory,
    grapt_schema::tri_state_ledger::U3aModelDefaultLocationWorkspace
);
location_query!(
    agent_list,
    AgentLocationQueryDto,
    AgentLocationQueryValueDto,
    grapt_schema::tri_state_ledger::U3aAgentLocation,
    grapt_schema::tri_state_ledger::U3aAgentLocationDirectory,
    grapt_schema::tri_state_ledger::U3aAgentLocationWorkspace
);

/// Decode the global permission list's frozen `LocationQuery` contract.
///
/// The wire algebra is intentionally shared with `project/current`; the
/// permission-specific application variant is what prevents a transport
/// refactor from silently dropping the selected location.
pub fn permission_list(raw: Option<&str>) -> Result<PermissionLocationQueryDto, QueryError> {
    project_current(raw)
}

pub fn session_list(raw: Option<&str>) -> Result<SessionListQueryDto, QueryError> {
    let mut pairs = QueryPairs::parse(raw)?;
    let workspace = session_workspace(pairs.take("workspace"))?;
    let limit = tri::<DtoString, grapt_schema::tri_state_ledger::U3aSessionListLimit, _>(
        pairs.take("limit"),
        "limit",
        dto_string,
    )?;
    let order = tri::<PageOrderDto, grapt_schema::tri_state_ledger::U3aSessionListOrder, _>(
        pairs.take("order"),
        "order",
        page_order,
    )?;
    let search = tri::<BoundedString<1024>, grapt_schema::tri_state_ledger::U3aSessionListSearch, _>(
        pairs.take("search"),
        "search",
        bounded_string,
    )?;
    let parent_id = match pairs.take("parentID") {
        None => OptionalNullable::absent(),
        Some(value) if value == "null" => {
            OptionalNullable::value(ParentFilterDto::Root(ParentFilterNullDto::Null))
        }
        Some(value) => {
            OptionalNullable::value(ParentFilterDto::Session(wire_id(value, "parentID")?))
        }
    };
    let directory = tri::<DtoString, grapt_schema::tri_state_ledger::U3aSessionListDirectory, _>(
        pairs.take("directory"),
        "directory",
        dto_string,
    )?;
    let project = tri::<
        WireId<grapt_types::ProjectId>,
        grapt_schema::tri_state_ledger::U3aSessionListProject,
        _,
    >(pairs.take("project"), "project", wire_id)?;
    let subpath = tri::<DtoString, grapt_schema::tri_state_ledger::U3aSessionListSubpath, _>(
        pairs.take("subpath"),
        "subpath",
        dto_string,
    )?;
    let cursor = tri::<DtoString, grapt_schema::tri_state_ledger::U3aSessionListCursor, _>(
        pairs.take("cursor"),
        "cursor",
        dto_string,
    )?;
    pairs.finish()?;
    Ok(SessionListQueryDto::new(
        workspace, limit, order, search, parent_id, directory, project, subpath, cursor,
    ))
}

pub fn message_list(
    raw: Option<&str>,
) -> Result<(MessageListQueryDto, MessageListView), QueryError> {
    let mut pairs = QueryPairs::parse(raw)?;
    let view = match pairs.take("diagnostics").as_deref() {
        None => MessageListView::Durable,
        Some("cache") => MessageListView::LiveCache,
        Some(_) => {
            return Err(QueryError::InvalidValue {
                field: "diagnostics",
            })
        }
    };
    let limit = tri::<DtoString, grapt_schema::tri_state_ledger::U3aMessageListLimit, _>(
        pairs.take("limit"),
        "limit",
        dto_string,
    )?;
    let order = tri::<PageOrderDto, grapt_schema::tri_state_ledger::U3aMessageListOrder, _>(
        pairs.take("order"),
        "order",
        page_order,
    )?;
    let cursor = tri::<DtoString, grapt_schema::tri_state_ledger::U3aMessageListCursor, _>(
        pairs.take("cursor"),
        "cursor",
        dto_string,
    )?;
    pairs.finish()?;
    Ok((MessageListQueryDto::new(limit, order, cursor), view))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_catalog_context_is_typed_and_duplicates_are_refused() {
        let query = project_current(Some(
            "session=ses_saved&location%5Bdirectory%5D=%2Fwork%2Frepo",
        ))
        .unwrap_or_else(|error| panic!("query: {error:?}"));
        assert_eq!(
            query
                .session()
                .as_present()
                .map(|id| id.as_inner().as_str()),
            Some("ses_saved")
        );
        for raw in [
            "session=",
            "session=bad%2Fid",
            "session=ses_a&session=ses_b",
        ] {
            assert!(project_current(Some(raw)).is_err());
        }
    }

    #[test]
    fn cache_diagnostics_are_an_explicit_bounded_history_view() {
        for (query, expected) in [
            (None, MessageListView::Durable),
            (Some("order=asc"), MessageListView::Durable),
            (
                Some("order=asc&diagnostics=cache"),
                MessageListView::LiveCache,
            ),
        ] {
            assert_eq!(message_list(query).map(|(_, view)| view), Ok(expected));
        }
        for value in ["", "null", "all", "true"] {
            assert_eq!(
                message_list(Some(&format!("diagnostics={value}"))).map(|(_, view)| view),
                Err(QueryError::InvalidValue {
                    field: "diagnostics"
                }),
            );
        }
        assert_eq!(
            message_list(Some("diagnostics=cache&diagnostics=cache")).map(|(_, view)| view),
            Err(QueryError::DuplicateField),
        );
        assert_eq!(
            message_list(Some("diagnostics=cache&surprise=1")).map(|(_, view)| view),
            Err(QueryError::UnknownField),
        );
    }

    #[test]
    fn event_location_accepts_only_the_frozen_deep_object_forms() {
        assert_eq!(event_location(None), Ok(EventLocationQuery::Absent));
        assert_eq!(
            event_location(Some("location=null")),
            Ok(EventLocationQuery::Null)
        );
        let nested = event_location(Some(
            "location%5Bdirectory%5D=null&location%5Bworkspace%5D=wrk_event",
        ));
        match nested {
            Ok(EventLocationQuery::Value {
                directory,
                workspace,
            }) => {
                assert_eq!(directory.as_ref().map(DtoString::as_str), Some("null"));
                assert_eq!(
                    workspace.as_ref().map(|value| value.as_inner().to_string()),
                    Some(String::from("wrk_event"))
                );
            }
            other => panic!("event location decoded to the wrong shape: {other:?}"),
        }
        assert!(event_location(Some("surprise=x")).is_err());
        assert!(event_location(Some("location=x")).is_err());
        assert!(event_location(Some("location=null&location%5Bdirectory%5D=work")).is_err());
    }

    #[test]
    fn session_workspace_obeys_ratified_absent_null_or_wrk_contract() {
        let absent = match session_list(None) {
            Ok(decoded) => decoded,
            Err(error) => panic!("absent workspace was refused: {error:?}"),
        };
        assert!(absent.workspace().is_absent(), "workspace absence was lost");

        let explicit_null = match session_list(Some("workspace=null")) {
            Ok(decoded) => decoded,
            Err(error) => panic!("workspace null sentinel was refused: {error:?}"),
        };
        assert!(
            explicit_null.workspace().is_null(),
            "workspace null sentinel did not decode as explicit null"
        );

        let workspace = match session_list(Some("workspace=wrk_query")) {
            Ok(decoded) => decoded,
            Err(error) => panic!("workspace value was refused: {error:?}"),
        };
        assert_eq!(
            workspace.workspace().as_value().map(WorkspaceIdDto::as_str),
            Some("wrk_query"),
            "workspace populated value was lost or rewritten"
        );

        for invalid in [
            "workspace=banana",
            "workspace=ses_wrong",
            "workspace=+",
            "workspace=",
        ] {
            assert_eq!(
                session_list(Some(invalid)),
                Err(QueryError::InvalidValue { field: "workspace" }),
                "non-sentinel/non-^wrk workspace escaped validation: {invalid}"
            );
        }
    }

    #[test]
    fn permission_location_query_preserves_default_explicit_and_malformed_distinctions() {
        let absent = permission_list(None)
            .unwrap_or_else(|error| panic!("absent permission location refused: {error:?}"));
        assert!(absent.location().is_absent());

        let explicit = permission_list(Some("location%5Bdirectory%5D=%2Fwork%2Fproduct"))
            .unwrap_or_else(|error| panic!("explicit permission location refused: {error:?}"));
        let directory = explicit
            .location()
            .as_value()
            .and_then(|location| location.directory().as_value())
            .map(DtoString::as_str);
        assert_eq!(directory, Some("/work/product"));

        assert_eq!(
            permission_list(Some("location=malformed")),
            Err(QueryError::InvalidValue { field: "location" })
        );
        assert_eq!(
            permission_list(Some("surprise=true")),
            Err(QueryError::UnknownField)
        );
    }

    #[test]
    fn literal_null_is_preserved_for_six_ordinary_session_query_strings() {
        let decoded = match session_list(Some(
            "limit=null&search=null&directory=null&project=null&subpath=null&cursor=null",
        )) {
            Ok(decoded) => decoded,
            Err(error) => panic!("literal strings were refused: {error:?}"),
        };
        assert_eq!(
            decoded.limit().as_value().map(DtoString::as_str),
            Some("null")
        );
        assert_eq!(
            decoded.search().as_value().map(BoundedString::as_str),
            Some("null")
        );
        assert_eq!(
            decoded.directory().as_value().map(DtoString::as_str),
            Some("null")
        );
        assert_eq!(
            decoded
                .project()
                .as_value()
                .map(|value| value.as_inner().to_string()),
            Some(String::from("null"))
        );
        assert_eq!(
            decoded.subpath().as_value().map(DtoString::as_str),
            Some("null")
        );
        assert_eq!(
            decoded.cursor().as_value().map(DtoString::as_str),
            Some("null")
        );
        assert!(
            session_list(Some("order=null")).is_err(),
            "literal null became an enum null instead of a string validation refusal"
        );

        let parent = match session_list(Some("parentID=null")) {
            Ok(parent) => parent,
            Err(error) => panic!("parent sentinel was refused: {error:?}"),
        };
        assert!(matches!(
            parent.parent_id().as_value(),
            Some(ParentFilterDto::Root(ParentFilterNullDto::Null))
        ));
    }
}

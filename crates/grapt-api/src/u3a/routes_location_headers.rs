//! Retained global-form clients carry location in headers on per-form calls.
use axum::http::HeaderMap;
use grapt_schema::OptionalNullable;

use super::super::{
    query::{self, QueryError, MAX_QUERY_BYTES},
    DtoString, ProjectCurrentLocationQueryDto, ProjectCurrentLocationQueryValueDto, WireId,
};

const DIRECTORY: &str = "x-grapt-directory";
const WORKSPACE: &str = "x-grapt-workspace";
/// The pre-rename spellings, still accepted from older renderers.
const LEGACY_DIRECTORY: &str = "x-secode-directory"; // grapt-rename: keep
const LEGACY_WORKSPACE: &str = "x-secode-workspace"; // grapt-rename: keep

/// The current header, or its pre-rename spelling when only that one is sent.
fn either(
    headers: &HeaderMap,
    current: &'static str,
    legacy: &'static str,
) -> Result<Option<String>, QueryError> {
    // Both spellings may be sent only when they agree.
    if headers.contains_key(current)
        && headers.contains_key(legacy)
        && headers.get(current) != headers.get(legacy)
    {
        return Err(QueryError::DuplicateField);
    }
    header(
        headers,
        if headers.contains_key(current) {
            current
        } else {
            legacy
        },
    )
}

fn header(headers: &HeaderMap, name: &'static str) -> Result<Option<String>, QueryError> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(QueryError::DuplicateField);
    }
    if value.len() > MAX_QUERY_BYTES {
        return Err(QueryError::TooLarge);
    }
    let value = value.to_str().map_err(|_| QueryError::InvalidEncoding)?;
    if name == DIRECTORY || name == LEGACY_DIRECTORY {
        // decodeURIComponent preserves literal '+', unlike query decoding.
        query::percent_decode(&value.replace('+', "%2B")).map(Some)
    } else {
        Ok(Some(value.to_owned()))
    }
}

pub(super) fn global_form(
    raw: Option<&str>,
    headers: &HeaderMap,
) -> Result<ProjectCurrentLocationQueryDto, QueryError> {
    let parsed = query::project_current(raw)?;
    if parsed.location().is_null() {
        return Ok(parsed);
    }
    let location = parsed.location().as_value();
    let mut directory = location
        .map(|value| value.directory().clone())
        .unwrap_or_else(OptionalNullable::absent);
    let mut workspace = location
        .map(|value| value.workspace().clone())
        .unwrap_or_else(OptionalNullable::absent);
    if directory.is_absent() {
        if let Some(value) = either(headers, DIRECTORY, LEGACY_DIRECTORY)? {
            directory = OptionalNullable::value(DtoString::try_new(value).map_err(|_| {
                QueryError::InvalidValue {
                    field: "location[directory]",
                }
            })?);
        }
    }
    if workspace.is_absent() {
        if let Some(value) = either(headers, WORKSPACE, LEGACY_WORKSPACE)? {
            workspace =
                OptionalNullable::value(WireId::parse(value).ok_or(QueryError::InvalidValue {
                    field: "location[workspace]",
                })?);
        }
    }
    if directory.is_absent() && workspace.is_absent() {
        return Ok(parsed);
    }
    Ok(ProjectCurrentLocationQueryDto::new(
        OptionalNullable::value(ProjectCurrentLocationQueryValueDto::new(
            directory, workspace,
        )),
        parsed.session().clone(),
    ))
}

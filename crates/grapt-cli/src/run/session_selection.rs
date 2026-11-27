//! Choose a usable resume target without confusing unreadable with absent.
use super::RunError;
use crate::ApiClient;
use grapt_types::{ProjectId, SessionId};
use std::collections::BTreeSet;

/// At most 10,000 rows with ordinary 50-row pages; exhaustion is a refusal,
/// never evidence that no session exists.
const MAX_SELECTION_PAGES: u16 = 200;

pub(super) async fn newest_for_project(
    client: &ApiClient,
    project: Option<&ProjectId>,
) -> Result<SessionId, RunError> {
    let mut unreadable = false;
    let mut seen = BTreeSet::new();
    let mut page = client.top_level_sessions().await?;
    for page_number in 0..MAX_SELECTION_PAGES {
        let rows = page.data().get().as_slice();
        for session in rows.iter().filter(|session| {
            project.is_none_or(|project| session.project_id().get().as_inner() == project)
        }) {
            if session.metadata_status().as_present().is_none() {
                return Ok(session.id().get().as_inner().clone());
            }
            unreadable = true;
        }
        let Some(next) = page.cursor().get().next().as_value() else {
            return Err(if unreadable {
                RunError::SessionMetadataUnavailable
            } else {
                RunError::NoSession
            });
        };
        // A page can be empty and still continue: the server skipped rows
        // it could not list. Repeated cursors and the page cap bound this.
        if !seen.insert(next.as_str().to_owned()) || page_number + 1 == MAX_SELECTION_PAGES {
            return Err(RunError::SessionSelectionIncomplete);
        }
        page = client.top_level_sessions_at(next.as_str()).await?;
    }
    Err(RunError::SessionSelectionIncomplete)
}

//! Picker requests use the existing client; a successful acknowledgement owns removal.
use super::*;
use crate::sessions::{Message, Request, Search};

pub(super) fn status<C: grapt_client::Connection>(
    client: &Client<C>,
    id: &SessionWireId,
) -> String {
    client
        .view_of(id)
        .filter(|_| Some(id) == client.selected())
        .and_then(|v| match v.idle {
            Some(true) => Some("idle".into()),
            Some(false) => Some(
                client
                    .observed_activity(id)
                    .map(|s| s.label().into_owned())
                    .unwrap_or_else(|| "running".into()),
            ),
            None => match &v.execution {
                grapt_client::ExecutionState::Idle => None,
                grapt_client::ExecutionState::Running { .. } => Some("running".into()),
                grapt_client::ExecutionState::Settled { outcome, .. } => {
                    Some(format!("{outcome:?}").to_lowercase())
                }
            },
        })
        .or_else(|| client.observed_activity(id).map(|s| s.label().into_owned()))
        .unwrap_or_else(|| "unreported".into())
}
pub(super) fn search<C: grapt_client::Connection>(
    client: &mut Client<C>,
    query: String,
) -> Message {
    let result = client
        .search_sessions(&query)
        .map(|rows| {
            rows.into_iter()
                .map(|row| Agent {
                    display_name: None,
                    current: client.selected() == Some(&row.id),
                    status: status(client, &row.id),
                    row,
                    context: Default::default(),
                })
                .collect()
        })
        .map_err(|e| e.to_string());
    Message::Search(Search { query, result })
}
pub(super) fn manage<C: grapt_client::Connection>(
    client: &mut Client<C>,
    request: Request,
) -> Message {
    let result = match &request {
        Request::Rename { id, title } => client.rename_session(id, title),
        Request::Delete { id } => client.delete_session(id),
    }
    .map_err(|e| e.to_string());
    let refresh_error = if result.is_ok() {
        client.refresh_sessions().err().map(|e| e.to_string())
    } else {
        None
    };
    Message::Managed {
        request,
        result,
        refresh_error,
    }
}

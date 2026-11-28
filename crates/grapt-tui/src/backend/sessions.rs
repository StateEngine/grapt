//! Picker requests use the existing client; a successful acknowledgement owns removal.
use super::command_lane::Turn;
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
/// The picker's message for a search's answer.
pub(super) fn searched<C: grapt_client::Connection>(
    client: &Client<C>,
    query: String,
    found: Result<grapt_client::Done, grapt_client::ClientError>,
) -> Message {
    let mut reach = grapt_client::CatalogReach::Read;
    let result = match found {
        Ok(grapt_client::Done::Found(found)) => {
            reach = found.reach;
            Ok(found
                .rows
                .into_iter()
                .map(|row| Agent {
                    display_name: None,
                    current: client.selected() == Some(&row.id),
                    status: status(client, &row.id),
                    row,
                    context: Default::default(),
                })
                .collect())
        }
        Ok(_) => Err("Session search answered without rows".into()),
        Err(error) => Err(error.to_string()),
    };
    Message::Search(Search {
        query,
        result,
        reach,
    })
}

/// A rename or delete on the command lane, then the catalog re-read that
/// follows an acknowledged one.
#[derive(Debug)]
pub(super) struct Manage {
    request: Request,
    acknowledged: bool,
}

pub(super) fn manage<C: grapt_client::Connection>(
    client: &mut Client<C>,
    request: Request,
) -> Turn<Manage, Message> {
    let call = match &request {
        Request::Rename { id, title } => client.prepare_rename(id, title),
        Request::Delete { id } => client.prepare_delete(id),
    };
    let manage = Manage {
        request,
        acknowledged: false,
    };
    Turn::Step(manage, call.map(grapt_client::Accepted::Next))
}

impl Manage {
    /// Only an acknowledged request re-reads the catalog; its outcome and the
    /// re-read's are reported apart.
    pub(super) fn advance<C: grapt_client::Connection>(
        self,
        client: &mut Client<C>,
        done: Result<grapt_client::Done, grapt_client::ClientError>,
    ) -> Turn<Self, Message> {
        let request = self.request;
        match (self.acknowledged, done) {
            (false, Ok(_)) => {
                let next = client
                    .prepare_sessions_refresh()
                    .map(grapt_client::Accepted::Next);
                let manage = Self {
                    request,
                    acknowledged: true,
                };
                Turn::Step(manage, next)
            }
            (false, Err(error)) => Turn::Settle(Message::Managed {
                request,
                result: Err(error.to_string()),
                refresh_error: None,
            }),
            (true, refreshed) => Turn::Settle(Message::Managed {
                request,
                result: Ok(()),
                refresh_error: refreshed.err().map(|error| error.to_string()),
            }),
        }
    }
}

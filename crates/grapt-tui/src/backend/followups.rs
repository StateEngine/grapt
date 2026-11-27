//! Queue admission shares Client's existing stable input identity and retry owner.
use super::CommandId;
use crate::followups::{Custody, Request};
use grapt_client::{wire::SessionWireId, Client, ClientError, Connection};

#[derive(Default)]
pub(super) struct Submissions {
    /// An unconfirmed follow-up keeps the command that sent it until it
    /// settles.
    retry: Option<(SessionWireId, CommandId)>,
    pub result: Option<(CommandId, Custody)>,
}

impl Submissions {
    pub fn submit<C: Connection>(
        &mut self,
        client: &mut Client<C>,
        request: Request,
        ready: bool,
        origin: CommandId,
    ) -> Result<(), ClientError> {
        let result = if !ready || client.selected() != Some(&request.session) {
            Err(ClientError::NotOffered {
                what: "Session changed or is not ready; queued follow-up kept",
            })
        } else if client.has_unconfirmed_prompt() {
            Err(ClientError::UnconfirmedEdit)
        } else {
            client.submit_queued_prompt(&request.text, &request.files)
        };
        let custody = if result.is_ok() {
            Custody::Accepted
        } else if client.selected() == Some(&request.session)
            && client.has_unconfirmed_prompt()
            && matches!(&result, Err(ClientError::Unconfirmed))
        {
            self.retry = Some((request.session, origin));
            Custody::Unconfirmed
        } else {
            Custody::Refused
        };
        self.result = Some((origin, custody));
        result
    }

    pub fn reconcile<C: Connection>(&mut self, client: &Client<C>) -> bool {
        if let Some((session, origin)) = &self.retry {
            if client.selected() == Some(session) && !client.has_unconfirmed_prompt() {
                self.result = Some((*origin, Custody::Accepted));
                self.retry = None;
                return true;
            }
        }
        false
    }
}

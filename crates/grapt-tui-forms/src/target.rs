//! Explicit request identities for elicitation forms.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Modified by Grapt: canonical Session or resolved location, never an inferred owner.
use super::*;
use grapt_client::wire::{FormLocation, FormOwner};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Session(SessionWireId),
    Location(FormLocation),
}
impl Target {
    pub fn owner_matches(&self, row: &FormRow) -> bool {
        match self {
            Self::Session(session) => row.owner.session() == Some(session),
            Self::Location(_) => row.owner == FormOwner::Global,
        }
    }
    pub fn active(&self, session: Option<&SessionWireId>, location: Option<&FormLocation>) -> bool {
        match self {
            Self::Session(expected) => session == Some(expected),
            Self::Location(expected) => location == Some(expected),
        }
    }
    pub(super) fn for_row(
        row: &FormRow,
        session: Option<&SessionWireId>,
        location: Option<&FormLocation>,
    ) -> Option<Self> {
        match &row.owner {
            FormOwner::Session(owner) if Some(owner) == session => {
                Some(Self::Session(owner.clone()))
            }
            FormOwner::Global => location.cloned().map(Self::Location),
            _ => None,
        }
    }
}

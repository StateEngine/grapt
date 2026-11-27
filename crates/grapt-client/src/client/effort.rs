//! Copyright 2023-2026 SpaceXAI.
//! Apache-2.0. Modified by Grapt: typed coordinates, existing atomic route,
//! explicit unsupported-level refusal and a single launch application.
//! Launch effort shares the existing atomic model-selection route.
use super::*;

#[derive(Debug)]
pub(super) struct LaunchEffort {
    pub(super) level: String,
    pub(super) session: Option<SessionWireId>,
}

impl<C: Connection> Client<C> {
    pub fn set_launch_effort(&mut self, effort: Option<String>) {
        self.launch_effort = effort.map(|level| LaunchEffort {
            level,
            session: None,
        });
    }

    pub(super) fn effort_choice(
        &self,
        model: &crate::wire::ModelRow,
    ) -> Result<crate::wire::ModelRow, ClientError> {
        if model.variant.is_none() {
            if let Some(level) = self
                .launch_effort
                .as_ref()
                .map(|effort| effort.level.as_str())
                .filter(|value| *value != "default")
            {
                let effort = crate::wire::ModelVariantWireId::parse(level.to_owned())
                    .ok_or_else(|| ClientError::ReasoningEffort(level.to_owned()))?;
                return self
                    .models
                    .iter()
                    .find(|row| {
                        row.id == model.id
                            && row.provider_id == model.provider_id
                            && row.variant.as_ref() == Some(&effort)
                    })
                    .cloned()
                    .ok_or_else(|| ClientError::ReasoningEffort(level.to_owned()));
            }
        }
        Ok(model.clone())
    }

    pub(super) fn refresh_selected_model(
        &mut self,
        session: &SessionWireId,
    ) -> Result<(), ClientError> {
        self.model = None;
        let bytes = self.ingest(Operation::SessionGet, Some(session), None, None, None, None)?;
        let selected = crate::wire::decode_session_model(&bytes)?;
        self.model = selected
            .map(|model| {
                self.models
                    .iter()
                    .find(|row| {
                        row.id == model.id
                            && row.provider_id == model.provider_id
                            && row.variant == model.variant
                    })
                    .cloned()
                    .unwrap_or(model)
            })
            .or_else(|| self.default_model.clone());
        Ok(())
    }

    pub(super) fn apply_launch_effort(&mut self) -> Result<(), ClientError> {
        if self.launch_effort.is_none() {
            return Ok(());
        }
        if self
            .launch_effort
            .as_ref()
            .and_then(|effort| effort.session.as_ref())
            .is_some_and(|session| self.selected.as_ref() != Some(session))
        {
            return Err(ClientError::NotOffered {
                what: "reasoning effort for another launch session",
            });
        }
        let model = self
            .model
            .clone()
            .or_else(|| {
                self.launch_effort
                    .as_ref()
                    .filter(|effort| effort.session.is_none())
                    .and(self.default_model.clone())
            })
            .ok_or(ClientError::SelectionIncomplete)?;
        let mut model = model;
        // The launch flag explicitly overrides this session's previous effort.
        model.variant = None;
        self.select_model(&model)
    }
}

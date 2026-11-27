//! MCP forms retain their location owner; no Session coordinate is invented.
use super::*;
use crate::wire::{FormLocation, FormOwner, FormRow, FormState, FormValue, FormWireId};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default)]
pub(super) struct Retained {
    location: Option<FormLocation>,
    rows: Vec<FormRow>,
}

impl Retained {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    fn at(&self, location: Option<&FormLocation>) -> &[FormRow] {
        if location.is_some() && self.location.as_ref() == location {
            &self.rows
        } else {
            &[]
        }
    }

    pub(super) fn startup_wait(
        &self,
        location: &FormLocation,
        existing: &BTreeSet<FormWireId>,
    ) -> bool {
        self.at(Some(location)).iter().any(|row| {
            row.owner == FormOwner::Global
                && !existing.contains(&row.id)
                && row
                    .metadata
                    .get("startup")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                && row
                    .metadata
                    .get("startupPending")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                && matches!(
                    row.metadata.get("kind").and_then(serde_json::Value::as_str),
                    Some("mcp-elicitation" | "mcp-url-elicitation")
                )
        })
    }
}

impl<C: Connection> Client<C> {
    /// Coalesced read hint from the location-scoped form feed or attachment.
    /// The caller still obtains every displayed row from the ordinary list.
    pub fn take_location_form_refresh(&mut self) -> bool {
        std::mem::take(&mut self.location_forms_dirty)
    }

    pub fn form_location(&self) -> Option<FormLocation> {
        self.project().map(|project| FormLocation {
            project: project.id.clone(),
            directory: self
                .catalog_refresh
                .binding
                .as_ref()
                .filter(|bound| &bound.project == project)
                .map_or_else(|| project.worktree.clone(), |bound| bound.directory.clone()),
        })
    }
    pub fn location_forms(&self) -> &[FormRow] {
        self.location_forms.at(self.form_location().as_ref())
    }

    pub fn refresh_location_forms(&mut self) -> Result<(), ClientError> {
        let Some(location) = self.form_location() else {
            self.location_forms.clear();
            return Ok(());
        };
        let bytes = self.ingest(
            Operation::FormRequestList,
            None,
            None,
            None,
            Some(&location.directory),
            None,
        )?;
        self.accept_location_forms(location, &bytes)
    }

    /// Install a location answer, refusing one for another location.
    pub(super) fn accept_location_forms(
        &mut self,
        location: crate::wire::FormLocation,
        bytes: &[u8],
    ) -> Result<(), ClientError> {
        let (actual, rows) = crate::wire::decode_location_forms(bytes)?;
        if actual != location {
            return Err(crate::wire::WireError::Invalid {
                what: "form location",
                value: "response location differs from requested location".into(),
            }
            .into());
        }
        self.install_location_form_rows(actual, rows);
        Ok(())
    }

    /// Install an already validated location snapshot without a request.
    pub(super) fn install_location_form_rows(&mut self, actual: FormLocation, rows: Vec<FormRow>) {
        self.location_forms = Retained {
            location: Some(actual),
            rows: rows
                .into_iter()
                .filter(|row| !matches!(row.owner, FormOwner::Session(_)))
                .take(self.bounds.max_retained_permissions)
                .collect(),
        };
    }

    pub fn location_form(
        &mut self,
        location: &FormLocation,
        form: &FormWireId,
    ) -> Result<FormRow, ClientError> {
        let bytes = self.ingest(
            Operation::GlobalFormGet,
            None,
            Some(form),
            None,
            Some(&location.directory),
            None,
        )?;
        let row = crate::wire::decode_form(&bytes)?;
        if row.id != *form || row.owner != FormOwner::Global {
            return Err(crate::wire::WireError::Invalid {
                what: "location form owner",
                value: "response does not identify the requested global form".into(),
            }
            .into());
        }
        Ok(row)
    }
    pub fn location_form_state(
        &mut self,
        location: &FormLocation,
        form: &FormWireId,
    ) -> Result<FormState, ClientError> {
        let bytes = self.ingest(
            Operation::GlobalFormState,
            None,
            Some(form),
            None,
            Some(&location.directory),
            None,
        )?;
        Ok(crate::wire::decode_form_state(&bytes)?)
    }
    pub fn answer_location_form(
        &mut self,
        location: &FormLocation,
        form: &FormWireId,
        answer: &BTreeMap<String, FormValue>,
    ) -> Result<(), ClientError> {
        let body = crate::wire::encode_form_reply(answer)?;
        self.ingest(
            Operation::GlobalFormReply,
            None,
            Some(form),
            Some(body),
            Some(&location.directory),
            None,
        )?;
        self.location_forms.rows.retain(|row| row.id != *form);
        self.refresh_location_forms()
    }
    pub fn cancel_location_form(
        &mut self,
        location: &FormLocation,
        form: &FormWireId,
    ) -> Result<(), ClientError> {
        self.ingest(
            Operation::GlobalFormCancel,
            None,
            Some(form),
            None,
            Some(&location.directory),
            None,
        )?;
        self.location_forms.rows.retain(|row| row.id != *form);
        self.refresh_location_forms()
    }
}

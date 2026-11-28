//! One bounded command read shares the existing catalog transport lane.
use super::*;
use crate::wire::{CommandTemplate, FormLocation};

#[derive(Debug, Default)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub(super) struct State {
    enabled: bool,
    hints: std::collections::BTreeSet<String>,
    hints_overflow: bool,
    dirty: bool,
    location: Option<FormLocation>,
    rows: Vec<CommandTemplate>,
    session: Option<SessionWireId>,
    scope: Option<String>,
    pending: bool,
    existing_forms: std::collections::BTreeSet<crate::wire::FormWireId>,
    settled: bool,
    note: Option<String>,
}
impl State {
    pub(super) fn accepts_scope(&self, scope: &str) -> bool {
        self.scope.as_deref().is_none_or(|current| current == scope)
    }
    pub(super) fn in_flight(&self) -> bool {
        self.pending
    }

    pub(super) fn reset(&mut self) {
        self.location = None;
        self.session = None;
        self.scope = None;
        self.dirty = false;
        self.rows.clear();
        self.pending = false;
        self.existing_forms.clear();
        self.settled = false;
        self.note = None;
    }
}
impl<C: Connection> Client<C> {
    pub(super) fn mcp_catalog_changed(&mut self, scope: &str) -> bool {
        self.mcp_resources_changed(scope);
        if self.command_catalog.scope.as_deref() == Some(scope) {
            return self.commands_changed();
        }
        if self.command_catalog.scope.is_none() && self.command_catalog.enabled {
            if self.command_catalog.hints.len() < 32 {
                self.command_catalog.hints.insert(scope.into());
            } else {
                self.command_catalog.hints_overflow = true;
            }
        }
        false
    }
    pub(super) fn commands_changed(&mut self) -> bool {
        if !self.command_catalog.enabled || self.keep_commands_refused() {
            return false;
        }
        if self.command_catalog.pending {
            self.command_catalog.dirty = true;
        } else {
            self.command_catalog.settled = false;
        }
        true
    }
    /// Reopening completion validates the files even if native watching failed.
    /// Keep the existing rows while the bounded read is in flight.
    pub fn open_commands(&mut self) {
        self.enable_commands();
        self.commands_changed();
    }
    pub fn enable_commands(&mut self) {
        if self.command_catalog.enabled {
            return;
        }
        self.command_catalog.enabled = true;
        if !self.keep_commands_refused() {
            self.command_catalog.note = Some("Commands loading…".into());
        }
    }
    pub fn commands(&self) -> &[CommandTemplate] {
        if self.command_catalog.location != self.form_location()
            || self.command_catalog.session != self.selected
        {
            return &[];
        }
        &self.command_catalog.rows
    }
    pub fn commands_status(&self) -> Option<&str> {
        if self.command_catalog.enabled
            && (self.command_catalog.location != self.form_location()
                || self.command_catalog.session != self.selected)
        {
            return Some("Commands loading…");
        }
        self.command_catalog.note.as_deref()
    }
    pub(super) fn commands_pending(&self) -> bool {
        self.command_catalog.enabled
            && self.form_location().is_some()
            && (!self.command_catalog.settled
                || (self.command_catalog.location != self.form_location()
                    || self.command_catalog.session != self.selected))
    }
    pub fn refresh_commands(&mut self) {
        if self.keep_commands_refused() {
            return;
        }
        if self.command_catalog.pending {
            self.connection.cancel_catalog();
        }
        self.command_catalog.reset();
    }
    /// Whether the service was refused; a command read it would owe
    /// settles with the refusal instead of loading.
    fn keep_commands_refused(&mut self) -> bool {
        match self.refused() {
            Ok(()) => false,
            Err(refusal) => {
                self.refuse_commands(&refusal.to_string());
                true
            }
        }
    }

    /// Whether the service was refused, so no command read will run.
    pub fn commands_refused(&self) -> bool {
        self.refused().is_err()
    }

    /// A refused service answers no command read: the catalog drops the rows
    /// it had, loaded or waiting, and settles saying why.
    pub(super) fn refuse_commands(&mut self, refusal: &str) {
        if self.command_catalog.pending {
            self.connection.cancel_catalog();
        }
        self.command_catalog.location = self.form_location();
        self.command_catalog.session = self.selected.clone();
        self.command_catalog.rows.clear();
        self.command_catalog.dirty = false;
        self.command_catalog.pending = false;
        self.command_catalog.settled = true;
        self.command_catalog.note = Some(format!("Commands unavailable: {refusal}"));
    }

    pub(super) fn advance_commands(&mut self) -> bool {
        if !self.command_catalog.enabled
            || self.catalog_pending()
            || (self.catalog_refresh.reading_repository() && !self.command_catalog.pending)
        {
            return false;
        }
        let location = self.form_location();
        let changed = self.command_catalog.location != location
            || self.command_catalog.session != self.selected;
        if changed {
            if self.command_catalog.pending {
                self.connection.cancel_catalog();
            }
            self.command_catalog.reset();
            self.command_catalog.location = location.clone();
            self.command_catalog.session = self.selected.clone();
        }
        let Some(location) = location else {
            return changed;
        };
        if self.command_catalog.settled {
            return changed;
        }
        let result = (|| -> Result<Option<crate::wire::CommandCatalog>, ClientError> {
            let response = if self.command_catalog.pending {
                self.connection.catalog_human_wait(
                    self.location_forms
                        .startup_wait(&location, &self.command_catalog.existing_forms),
                );
                self.connection.poll_catalog()?
            } else {
                self.command_catalog.existing_forms = self
                    .location_forms()
                    .iter()
                    .map(|row| row.id.clone())
                    .collect();
                let request = WireRequest::new(Operation::CommandInspect, None, None)?;
                let request = if let Some(session) = &self.selected {
                    request.with_query("session", crate::wire::Spelled::spelled(session))
                } else {
                    request.with_query("location[directory]", &location.directory)
                };
                self.connection.begin_catalog(&request)?
            };
            let Some(response) = response else {
                return Ok(None);
            };
            let bytes = self.accept_response(Operation::CommandInspect, response)?;
            let catalog = crate::wire::decode_commands(&bytes)?;
            if catalog.owner != location {
                return Err(crate::wire::WireError::Invalid {
                    what: "command location",
                    value: "response differs from selected workspace".into(),
                }
                .into());
            }
            Ok(Some(catalog))
        })();
        match result {
            Ok(None) => {
                self.command_catalog.pending = true;
                let loading = "Commands loading…";
                let changed = self.command_catalog.note.as_deref() != Some(loading);
                self.command_catalog.note = Some(loading.into());
                changed
            }
            Ok(Some(catalog)) => {
                let dirty = self.command_catalog.dirty
                    || self.command_catalog.hints_overflow
                    || catalog
                        .scope
                        .as_ref()
                        .is_some_and(|scope| self.command_catalog.hints.contains(scope));
                self.command_catalog.hints.clear();
                self.command_catalog.hints_overflow = false;
                self.command_catalog.dirty = false;
                self.command_catalog.rows = catalog.rows;
                self.command_catalog.scope = catalog.scope;
                self.command_catalog.pending = false;
                self.command_catalog.settled = !dirty;
                self.command_catalog.note =
                    (!catalog.warnings.is_empty()).then(|| catalog.warnings.join("\n"));
                true
            }
            Err(error) => {
                self.connection.cancel_catalog();
                self.command_catalog.rows.clear();
                self.command_catalog.pending = false;
                self.command_catalog.settled = true;
                self.command_catalog.note =
                    Some(format!("Commands unavailable; /commands retries: {error}"));
                true
            }
        }
    }
}

#[cfg(test)]
mod tests;

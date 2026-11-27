//! Attachment-scoped operations share recovery's one pending HTTP owner.
use super::*;
use crate::preset::{Job, Phase, Preset};

impl<C: Connection> Client<C> {
    /// Explicit launch intent requires successful selection and policy readback.
    pub fn launch_preset_ready(&self) -> Result<bool, ClientError> {
        if self.preset.launch.is_none() {
            return Ok(true);
        }
        if let Some(error) = &self.preset.error {
            return Err(ClientError::Preset(error.clone()));
        }
        Ok(self.preset.applied && self.preset.job.is_none())
    }

    /// Explicit startup intent. Reconnects only refresh its read-only display.
    pub fn set_launch_preset(&mut self, preset: Option<Preset>) {
        self.preset.launch = preset;
    }

    /// Read the selected and effective server policy without changing authority.
    pub fn refresh_permission_preset(&mut self) -> Result<(), ClientError> {
        if !self.preset.applied
            && self
                .preset
                .job
                .as_ref()
                .is_some_and(|job| matches!(job.phase, Phase::Apply(_)))
        {
            return Err(ClientError::Preset(
                "Launch permission selection is still applying; retry shortly.".into(),
            ));
        }
        let session = self.prepare_permission_preset_read()?;
        let result = self
            .ingest(
                Operation::SessionPresetGet,
                Some(&session),
                None,
                None,
                None,
                None,
            )
            .and_then(|bytes| Ok(crate::wire::decode_preset(&bytes)?));
        match result {
            Ok(snapshot) => {
                self.preset.snapshot = Some(snapshot);
                self.preset.error = (!self.preset.applied).then(||
                    "Permission selection is unconfirmed. Reopen the session before sending or changing permissions.".into());
                Ok(())
            }
            Err(error) => {
                self.preset.error = Some(error.to_string());
                Err(error)
            }
        }
    }

    /// Reuse the existing server selection route and read back effective policy.
    pub fn select_permission_preset(&mut self, preset: Preset) -> Result<(), ClientError> {
        self.require_launch_preset()?;
        let session = self.prepare_permission_preset_read()?;
        let body = crate::wire::encode_preset(preset)?;
        if let Err(error) = self.ingest(
            Operation::SessionPresetSelect,
            Some(&session),
            None,
            Some(body),
            None,
            None,
        ) {
            if super::submission::uncertain(&error) {
                self.preset.applied = false;
            }
            self.preset.error = Some(format!("Selection unconfirmed: {error}"));
            return Err(error);
        }
        self.preset.applied = true;
        self.refresh_permission_preset()
    }

    fn prepare_permission_preset_read(&mut self) -> Result<SessionWireId, ClientError> {
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        let generation = self
            .preset
            .generation
            .checked_add(1)
            .ok_or_else(|| ClientError::Preset("attachment count exhausted".into()))?;
        if self.preset.job.as_ref().is_some_and(|job| job.pending) {
            self.connection.cancel_read();
        }
        self.preset.generation = generation;
        self.preset.job = None;
        self.preset.error = None;
        Ok(session)
    }

    pub(super) fn attach_preset(&mut self, session: &SessionWireId) -> Result<(), ClientError> {
        self.preset.generation = self
            .preset
            .generation
            .checked_add(1)
            .ok_or_else(|| ClientError::Preset("attachment count exhausted".into()))?;
        self.preset.snapshot = None;
        self.preset.error = None;
        self.preset.applied = self.preset.launch.is_none();
        self.preset.job = Some(Job {
            session: session.clone(),
            generation: self.preset.generation,
            phase: self.preset.launch.map_or(Phase::Read, Phase::Apply),
            pending: false,
        });
        Ok(())
    }

    pub(super) fn preset_pending(&self) -> bool {
        self.preset.job.as_ref().is_some_and(|job| job.pending)
    }

    pub(super) fn refresh_preset_display(&mut self) {
        if self.preset.job.is_none() && self.preset.applied {
            if let Some(session) = self.selected.clone() {
                self.preset.job = Some(Job {
                    session,
                    generation: self.preset.generation,
                    phase: Phase::Read,
                    pending: false,
                });
            }
        }
    }

    pub(super) fn require_launch_preset(&self) -> Result<(), ClientError> {
        if self.preset.applied {
            return Ok(());
        }
        Err(ClientError::Preset(
            self.preset.error.clone().unwrap_or_else(|| {
                "Prompt kept until the launch preset is applied. Press Enter to send it.".into()
            }),
        ))
    }

    pub(super) fn advance_preset(&mut self) -> bool {
        if self
            .recovery
            .history
            .as_ref()
            .is_some_and(|history| history.pending)
        {
            return false;
        }
        let mut changed = false;
        // An embedded operation may complete immediately; sockets yield once.
        for _ in 0..2 {
            let Some(mut job) = self.preset.job.take() else {
                break;
            };
            if Some(&job.session) != self.selected.as_ref()
                || job.generation != self.preset.generation
            {
                if job.pending {
                    self.connection.cancel_read();
                }
                continue;
            }
            let operation = match job.phase {
                Phase::Apply(_) => Operation::SessionPresetSelect,
                Phase::Read => Operation::SessionPresetGet,
            };
            let result = (|| -> Result<Option<Vec<u8>>, ClientError> {
                let response = if job.pending {
                    self.connection.poll_read()?
                } else {
                    // Foreground: an older-history page gives way, and asks
                    // again from its cursor once the lane is free.
                    self.yield_backfill();
                    let mut request = WireRequest::new(operation, Some(&job.session), None)?;
                    if let Phase::Apply(preset) = job.phase {
                        request = request.with_body(crate::wire::encode_preset(preset)?);
                    }
                    self.connection.begin_read(&request)?
                };
                response
                    .map(|response| self.accept_response(operation, response))
                    .transpose()
            })();
            match result {
                Ok(None) => {
                    job.pending = true;
                    self.preset.job = Some(job);
                    break;
                }
                Ok(Some(bytes)) => {
                    changed = true;
                    match job.phase {
                        Phase::Apply(_) => {
                            self.preset.applied = true;
                            job.phase = Phase::Read;
                            job.pending = false;
                            self.preset.job = Some(job);
                        }
                        Phase::Read => match crate::wire::decode_preset(&bytes) {
                            Ok(snapshot) => {
                                self.preset.snapshot = Some(snapshot);
                                self.preset.error = None;
                            }
                            Err(error) => self.preset.error = Some(error.to_string()),
                        },
                    }
                }
                Err(error) => {
                    self.connection.cancel_read();
                    self.preset.error = Some(error.to_string());
                    changed = true;
                    break;
                }
            }
        }
        changed
    }

    /// The last effective server readback, retained through a failed refresh.
    pub fn bypass_permissions_on(&self) -> bool {
        self.selected.is_some()
            && self
                .preset
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.effective == Preset::Bypass)
    }

    pub fn preset_line(&self) -> Option<String> {
        self.selected.as_ref()?;
        if let Some(error) = &self.preset.error {
            let warning = if self
                .preset
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.effective == Preset::Bypass)
            {
                "Bypass permissions on (last confirmed) | "
            } else {
                ""
            };
            return Some(format!("{warning}Preset unavailable: {error}"));
        }
        let Some(snapshot) = &self.preset.snapshot else {
            return Some(
                if self.preset.applied {
                    "Preset: reading…"
                } else {
                    "Preset: applying…"
                }
                .into(),
            );
        };
        let selected = snapshot
            .selected
            .map(crate::preset::name)
            .unwrap_or("default");
        let busy = self.view().is_some_and(|view| {
            matches!(
                view.execution,
                crate::projection::ExecutionState::Running { .. }
            )
        });
        Some(format!(
            "{}Preset: {selected} | effective: {}{}",
            if snapshot.effective == Preset::Bypass {
                "Bypass permissions on | "
            } else {
                ""
            },
            crate::preset::name(snapshot.effective),
            if busy {
                " | applies from the next turn"
            } else {
                ""
            }
        ))
    }
}

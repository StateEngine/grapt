//! Attach the launcher's exact session, applying an explicit effort override once.
use super::*;

/// The saved choices a Session restores once it is selected.
pub(super) struct SavedChoice {
    model: Option<crate::wire::ModelRow>,
    wanted: (
        Option<crate::wire::AgentWireId>,
        Option<crate::wire::ModelRow>,
    ),
}

impl<C: Connection> Client<C> {
    /// I/O-free selection baseline shared with staged worktree attachment.
    /// Clear session-scoped caches before either caller installs target data.
    pub(super) fn prepare_session_selection(
        &mut self,
        session: &SessionWireId,
    ) -> Result<(), ClientError> {
        self.sandbox_worktree_config_preview = None;
        self.reset_history_window();
        self.promoted_inputs.clear();
        self.connection.cancel_read();
        self.recovery.history = None;
        // A gap belongs to the session being left. The new session's stream has
        // its own baseline, and carrying the old notice over would claim a
        // recovery this view never performed.
        self.gap_recovery_pending = false;
        // A resume continues one watched Session's stream, never another's.
        self.drop_resume();
        self.model = None;
        self.attach_preset(session)?;
        self.selected = Some(session.clone());
        // Tell the service which Session this window watches; the last window
        // to go is what stops its turn. The typed identity is handed on as it
        // is: this seam names a Session, it does not render one.
        self.connection
            .watch_session(Some(crate::wire::Typed::typed(session)));
        self.subagent_model = None;
        self.forms.clear();
        // The forms and asks just cleared are what a snapshot installed; until
        // this selection installs its own, the session is not ready again,
        // even when it is the session that was selected before.
        self.positions.forget_installed();
        self.location_forms_dirty = true;
        self.raise_session_forms(true);
        Ok(())
    }

    /// Interactive resume restores saved choices without reapplying a launch hint.
    pub fn resume_session(&mut self, session: &SessionWireId) -> Result<(), ClientError> {
        let effort = self
            .launch_effort
            .take()
            .filter(|effort| effort.session.is_none());
        let result = self.attach_saved_session(session, false);
        self.launch_effort = effort;
        result
    }

    /// Read the session before restoring its project, catalog choices and history.
    /// Missing or disabled saved choices keep the composer disabled; no default
    /// replaces them. An explicit launch effort is saved before submission.
    pub fn attach_launch_session(&mut self, session: &SessionWireId) -> Result<(), ClientError> {
        let effort = self.launch_effort.take();
        // A refused prepared launch cannot leave previous choices ready.
        self.clear_attachment();
        let result = self.attach_saved_session(session, effort.is_some());
        self.launch_effort = effort.map(|mut effort| {
            effort.session = Some(session.clone());
            effort
        });
        result?;
        if self.catalog_pending() {
            Ok(())
        } else {
            self.apply_launch_effort()
        }
    }

    fn attach_saved_session(
        &mut self,
        session: &SessionWireId,
        override_effort: bool,
    ) -> Result<(), ClientError> {
        let response = self.ingest(Operation::SessionGet, Some(session), None, None, None, None);
        if matches!(&response, Err(ClientError::Status { status: 404, .. })) {
            self.forget_session(session);
        }
        let bytes = response?;
        let mut saved = crate::wire::decode_session_attachment(&bytes)?;
        if &saved.id != session {
            return Err(ClientError::NotOffered {
                what: "requested session",
            });
        }
        let directory = saved.project.worktree.clone();
        if !self.launch_project.as_ref().is_some_and(|project| {
            project.id == saved.project.id && project.worktree == saved.project.worktree
        }) {
            // A saved Session keeps its working directory, which may be nested
            // inside its repository. Compare the resolved checkout roots, not
            // that nested cwd against the launcher's repository root.
            let bytes = self.ingest(
                Operation::ProjectCurrent,
                Some(session),
                None,
                None,
                Some(&saved.project.worktree),
                None,
            )?;
            let (id, worktree) = crate::wire::decode_project_current(&bytes)?;
            if id != saved.project.id {
                return Err(ClientError::NotOffered {
                    what: "session in saved project",
                });
            }
            saved.project.worktree = worktree;
        }
        if override_effort {
            if let Some(model) = &mut saved.model {
                model.variant = None;
            }
        }
        let choice = self.adopt_saved(session, saved, &bytes, directory)?;
        if let Err(error) = self.select(session) {
            self.model = None;
            return Err(error);
        }
        self.restore_saved_model(choice);
        Ok(())
    }

    /// Adopt a saved Session's project, catalog binding and agent, detaching
    /// the current one; the caller selects the Session. Refused, with nothing
    /// changed, when it is not in the launch directory.
    pub(super) fn adopt_saved(
        &mut self,
        session: &SessionWireId,
        saved: crate::wire::SessionAttachment,
        row: &[u8],
        directory: String,
    ) -> Result<SavedChoice, ClientError> {
        if self.launch_project.as_ref().is_some_and(|project| {
            project.id != saved.project.id || project.worktree != saved.project.worktree
        }) {
            return Err(ClientError::NotOffered {
                what: "session in launch directory",
            });
        }
        let row = crate::wire::decode_session(row)?;
        self.remember_session(SessionSummary {
            id: row.id,
            title: row.title,
            parent: row.parent,
            detail: row.detail,
        });
        // A failed lookup must not detach the current conversation.
        self.clear_attachment();
        let wanted = (saved.agent.clone(), saved.model.clone());
        self.catalog_refresh.binding = Some(catalog_refresh::Binding {
            session: session.clone(),
            project: saved.project.clone(),
            directory,
        });
        self.select_project(&saved.project)?;
        self.agent = saved.agent.filter(|id| self.saved_agent_offered(id));
        Ok(SavedChoice {
            model: saved.model,
            wanted,
        })
    }

    /// Restore a selected Session's saved model. Ordinary opens can retain an
    /// unavailable model or use a default; a prepared launch requires its
    /// complete saved choice to remain offered.
    pub(super) fn restore_saved_model(&mut self, choice: SavedChoice) {
        self.model = choice.model.and_then(|model| {
            self.models
                .iter()
                .find(|row| {
                    row.id == model.id
                        && row.provider_id == model.provider_id
                        && row.variant == model.variant
                        && row.enabled
                })
                .cloned()
        });
        if self.catalog_in_background() {
            self.remember_catalog_selection(choice.wanted.0, choice.wanted.1);
        }
    }

    pub(super) fn clear_attachment(&mut self) {
        self.sandbox_worktree_config_preview = None;
        self.catalog_refresh.binding = None;
        self.selected = None;
        self.agent = None;
        self.model = None;
        self.project = None;
        // Detaching abandons the recovery: no session is selected, so no
        // request_history can complete it, and a notice frozen on "refreshing"
        // would describe work that will never happen here.
        self.gap_recovery_pending = false;
        self.drop_resume();
    }
}

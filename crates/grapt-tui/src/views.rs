//! Immediate family presentation.
// Copyright 2025 OpenAI. Apache-2.0; modified for bounded recent rows and a
// ticketed attachment acknowledgement. The shared Client still owns all controls.
use crate::{
    backend::{Backend, Command, Preview, Selection},
    ui::Ui,
};
use grapt_client::{
    wire::{SessionWireId, Spelled},
    Slice,
};
use std::collections::VecDeque;

const MEMBERS: usize = 16;
pub(crate) const RECENT_ROWS: usize = 32;

#[derive(Default)]
pub struct Views {
    recent: VecDeque<Preview>,
    /// The Select command whose view the UI waits for.
    pending: Option<crate::backend::CommandId>,
}

impl Views {
    pub fn forget(&mut self, id: &SessionWireId) {
        self.recent.retain(|entry| &entry.session != id);
    }
    pub fn warm(&mut self, preview: Preview) {
        self.recent.retain(|old| old.session != preview.session);
        self.recent.push_back(Preview {
            session: preview.session,
            rows: preview
                .rows
                .into_iter()
                .rev()
                .take(RECENT_ROWS)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        });
        while self.recent.len() > MEMBERS {
            self.recent.pop_front();
        }
    }

    fn rows(&self, session: &SessionWireId) -> Vec<Slice> {
        self.recent
            .iter()
            .find(|p| &p.session == session)
            .map(|p| p.rows.clone())
            .unwrap_or_default()
    }

    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }

    /// Only the newest requested attachment can restore controls or replace rows.
    pub fn accept(&mut self, selection: Option<crate::backend::CommandId>) -> bool {
        if self.pending.is_none() {
            return true;
        }
        if self.pending != selection {
            return false;
        }
        self.pending = None;
        true
    }
}

impl Ui {
    /// Keep scroll intent until the authoritative history replaces the cached tail.
    pub fn advance_history(&mut self) -> Option<grapt_client::client::HistoryMove> {
        if self.views.loading() {
            return None;
        }
        self.navigation.advance(&mut self.transcript)
    }

    pub fn select_view(&mut self, backend: &Backend, id: SessionWireId) -> Result<(), String> {
        let current = self
            .agents
            .iter()
            .find(|a| a.current)
            .map(|a| a.row.id.clone());
        let selection =
            crate::send_tracked(backend, Command::Select(Selection { id: id.clone() }))?;
        if let Some(session) = current.filter(|_| !self.browsing) {
            self.views.warm(Preview {
                session,
                rows: self.transcript.recent_rows(RECENT_ROWS),
            });
        }
        self.views.pending = Some(selection);
        let agent = self.agents.iter().find(|a| a.row.id == id).cloned();
        self.session_update(
            Some(id.clone()),
            agent
                .as_ref()
                .is_some_and(|a| crate::subagents::running(&a.status)),
        );
        for member in &mut self.agents {
            member.current = member.row.id == id;
        }
        self.permission_update(None);
        self.forms = Default::default();
        self.queued = Default::default();
        self.usage = Default::default();
        self.policy = None;
        self.bypass_permissions = false;
        self.mail_attention = None;
        self.continuity_attention = None;
        self.feedback = None;
        self.failed_execution = None;
        self.model = None;
        self.models = None;
        self.picker = None;
        self.pending = None;
        self.prompt_ready = false;
        self.browsing = false;
        self.navigation.reset();
        self.launch.update(crate::launch::Readiness::Loading);
        self.title = agent
            .map(|a| a.row.title)
            .unwrap_or_else(|| id.spelled().into());
        self.status = "Loading conversation…; draft editable".into();
        self.transcript.update(true, 0, self.views.rows(&id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_client::wire::session_id;
    #[test]
    fn a_late_attachment_cannot_unlock_a_newer_view_or_consume_its_draft(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (commands, rx) = crate::command_channel();
        let updates = crate::backend::Handoff::default();
        let (wake, _) = std::sync::mpsc::sync_channel(1);
        let backend = Backend {
            commands,
            updates,
            wake,
        };
        let mut ui = Ui::new(false);
        ui.composer.insert_str("human draft");
        ui.select_view(&backend, session_id("ses_a")?)?;
        let (first, Command::Select(_)) = rx.try_envelope()? else {
            return Err("first select".into());
        };
        ui.select_view(&backend, session_id("ses_b")?)?;
        let (second, Command::Select(_)) = rx.try_envelope()? else {
            return Err("second select".into());
        };
        ui.navigation.scroll(40_000);
        assert_eq!(ui.advance_history(), None);
        assert_eq!(ui.transcript.scroll, 0);
        assert!(!ui.views.accept(Some(first)));
        assert!(ui.views.loading());
        assert!(!ui.launch.readiness.can_submit());
        assert!(ui.permission.is_none() && ui.model.is_none() && !ui.prompt_ready);
        assert_eq!(ui.composer.lines().join("\n"), "human draft");
        assert!(ui.views.accept(Some(second)));
        assert!(!ui.views.loading());
        assert_eq!(ui.advance_history(), None);
        assert_eq!(
            ui.transcript.scroll, 40_000,
            "attachment preserves queued scroll intent"
        );
        Ok(())
    }
}

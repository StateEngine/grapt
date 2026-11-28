//! Permission asks and forms: the two questions a run can stop to ask.
//!
//! Split from `client.rs` by SUBJECT when that file passed this workspace's
//! module ceiling. Every route, bound and refusal is unchanged.

use super::*;

/// Open asks built and bounded, not yet published.
pub(super) struct PermissionCandidate {
    asks: Vec<PermissionAsk>,
    dropped: usize,
}

impl<C: Connection> Client<C> {
    /// Pending forms for the selected session.
    pub fn forms(&self) -> &[crate::wire::FormRow] {
        &self.forms
    }

    /// Raise the hint to re-read the selected Session forms, stamping it.
    pub(super) fn raise_session_forms(&mut self, changed: bool) {
        if changed {
            self.session_forms_dirty = true;
            self.session_form_hints = self.session_form_hints.wrapping_add(1);
        }
    }

    /// Consume a coalesced hint to re-read the selected Session forms.
    pub fn take_session_form_refresh(&mut self) -> bool {
        std::mem::take(&mut self.session_forms_dirty)
    }

    /// Re-read the selected session's OUTSTANDING forms.
    ///
    /// ## What this list is, and is not
    ///
    /// It is the set of questions still awaiting an answer, which includes
    /// SUSPENDED ones. It is not the set of forms that exist: a settled form
    /// leaves this list and stays readable by its own identity through
    /// [`Client::form_state`].
    ///
    /// Settlement used to be impossible here and the renderer said so. The
    /// accepted backend now serves all seven form routes, so the honest
    /// statement changed with it: [`Client::answer_form`] and
    /// [`Client::cancel_form`] settle a form through the canonical protocol,
    /// and the renderer no longer has an absence to explain.
    pub fn refresh_forms(&mut self) -> Result<(), ClientError> {
        let Some(session) = self.selected.clone() else {
            self.forms.clear();
            self.session_forms_dirty = false;
            return Ok(());
        };
        let bytes = self.ingest(
            Operation::SessionFormList,
            Some(&session),
            None,
            None,
            None,
            None,
        )?;
        self.install_session_forms(&bytes)
    }

    pub(super) fn install_session_forms(&mut self, bytes: &[u8]) -> Result<(), ClientError> {
        self.forms = crate::wire::decode_session_forms(bytes)?
            .into_iter()
            .take(self.bounds.max_retained_permissions)
            .collect();
        self.session_forms_dirty = false;
        Ok(())
    }

    pub fn refresh_permissions(&mut self) -> Result<(), ClientError> {
        let bytes = self.ingest(
            Operation::PermissionRequestList,
            None,
            None,
            None,
            None,
            None,
        )?;
        self.accept_permissions(bytes)
    }

    pub(super) fn accept_permissions(&mut self, bytes: Vec<u8>) -> Result<(), ClientError> {
        let rows = crate::wire::decode_permissions(&bytes)?;
        self.accept_permission_rows(rows)
    }

    /// Replace the open asks with decoded rows, whichever response carried them.
    pub(super) fn accept_permission_rows(
        &mut self,
        rows: Vec<crate::wire::PermissionRow>,
    ) -> Result<(), ClientError> {
        let candidate = self.permission_candidate(rows, Vec::new());
        self.commit_permissions(candidate);
        Ok(())
    }

    /// The open asks `rows` would publish, then `kept` (asks retained from
    /// other sessions), within the bound, built without touching the client.
    pub(super) fn permission_candidate(
        &self,
        rows: Vec<crate::wire::PermissionRow>,
        kept: Vec<PermissionAsk>,
    ) -> PermissionCandidate {
        let mut dropped = self.diagnostics.permissions_dropped;
        let mut asks = Vec::new();
        for ask in rows.into_iter().map(ask).chain(kept) {
            if asks.len() >= self.bounds.max_retained_permissions {
                dropped = dropped.saturating_add(1);
                continue;
            }
            asks.push(ask);
        }
        PermissionCandidate { asks, dropped }
    }

    /// Publish a permission candidate; nothing here can fail.
    pub(super) fn commit_permissions(&mut self, candidate: PermissionCandidate) {
        self.permissions = candidate.asks;
        self.diagnostics.permissions_dropped = candidate.dropped;
    }

    /// Answer one permission ask through the complete typed reply vocabulary,
    /// then re-read the open asks.
    pub fn reply_permission(
        &mut self,
        session: &SessionWireId,
        request_id: &PermissionWireId,
        choice: crate::wire::PermissionReplyChoice,
    ) -> Result<(), ClientError> {
        self.send_permission_reply(session, request_id, choice)?;
        self.refresh_permissions()
    }

    /// Answer one permission ask without re-reading. Separate so a caller can
    /// tell a refused answer from an accepted one whose re-read failed.
    pub fn send_permission_reply(
        &mut self,
        session: &SessionWireId,
        request_id: &PermissionWireId,
        choice: crate::wire::PermissionReplyChoice,
    ) -> Result<(), ClientError> {
        let body = crate::wire::encode_permission_reply(request_id, choice)?;
        self.ingest(
            Operation::SessionPermissionReply,
            Some(session),
            Some(request_id),
            Some(body),
            None,
            None,
        )?;
        // The send-only API does not re-list, but its acknowledged settlement
        // answers this ask.
        self.permissions
            .retain(|ask| !(&ask.session_id == session && &ask.request_id == request_id));
        Ok(())
    }

    /// Read one form's settlement state through the canonical route.
    ///
    /// Separate from the outstanding list because the two answer different
    /// questions. The list says what still needs an answer; this says what
    /// became of ONE form, including after it left the list. A client with only
    /// the list would have to infer "answered" from a disappearance, which is
    /// the same observation a cancel, a suspension-then-settle, or a dropped
    /// connection produces.
    pub fn form_state(
        &mut self,
        session: &SessionWireId,
        form: &crate::wire::FormWireId,
    ) -> Result<crate::wire::FormState, ClientError> {
        let bytes = self.ingest(
            Operation::SessionFormState,
            Some(session),
            Some(form),
            None,
            None,
            None,
        )?;
        Ok(crate::wire::decode_form_state(&bytes)?)
    }

    /// Settle one form by answering it.
    ///
    /// A SUSPENDED form is answerable on exactly these terms — that is what
    /// makes suspension nonterminal rather than a slow cancel. Answering one
    /// settles the FORM; it does not resurrect the waiter that was lost with
    /// the process, and this client never claims it did.
    ///
    /// A second settlement is refused by the product with `409`, which arrives
    /// as [`ClientError::Status`] carrying that status rather than as a silent
    /// success. The caller is told it was too late instead of being shown an
    /// answer the product did not take.
    pub fn answer_form(
        &mut self,
        session: &SessionWireId,
        form: &crate::wire::FormWireId,
        answer: &std::collections::BTreeMap<String, crate::wire::FormValue>,
    ) -> Result<(), ClientError> {
        let body = crate::wire::encode_form_reply(answer)?;
        self.ingest(
            Operation::SessionFormReply,
            Some(session),
            Some(form),
            Some(body),
            None,
            None,
        )?;
        self.refresh_forms()
    }

    /// Settle one form by declining it.
    ///
    /// Cancelling is a DECISION, and the only other one this client can make.
    /// Leaving a form unanswered is not a third outcome: it stays open.
    pub fn cancel_form(
        &mut self,
        session: &SessionWireId,
        form: &crate::wire::FormWireId,
    ) -> Result<(), ClientError> {
        self.ingest(
            Operation::SessionFormCancel,
            Some(session),
            Some(form),
            None,
            None,
            None,
        )?;
        self.refresh_forms()
    }
}

/// One listed permission row as an open ask.
fn ask(row: crate::wire::PermissionRow) -> PermissionAsk {
    PermissionAsk {
        session_id: row.session_id,
        request_id: row.id,
        title: row.title,
        subtitle: row.subtitle,
        live: row.live,
        allow_always: row.allow_always,
        call: row.call,
        preview: row.preview,
    }
}

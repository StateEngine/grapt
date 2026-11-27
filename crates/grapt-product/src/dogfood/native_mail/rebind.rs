//! Refused credentials outlive metadata, presentation and transport shutdown.
//! The refusal belongs to the provision generation (see `CredentialBudget`),
//! so only a newer authenticated provision clears it.
use super::*;

impl NativeMailContext {
    /// No capability is held at all: only a provision can repair it.
    pub(super) fn require_rebind(&mut self) {
        self.adopt_binding();
        let held = self.credential.refusal();
        self.credential.absent();
        self.parked(held);
    }

    /// Apply one outcome stamped with the generation its binding read.
    pub(super) fn observe_credential(&mut self, event: CredentialEvent) -> Disposition {
        self.adopt_binding();
        let held = self.credential.refusal();
        let stamp = self.binding.as_ref().map(|binding| binding.generation);
        let disposition = self.credential.observe(stamp, event);
        // Live and transport outcomes carry no credential fact: they never
        // re-park, re-present or report a held refusal.
        if !matches!(event, CredentialEvent::Transport | CredentialEvent::Live) {
            match disposition {
                Disposition::Park(_) => self.parked(held),
                Disposition::Ignore => {
                    crate::diagnostics::report(format!("native mail ignored a credential outcome from another provision session={} event={event:?}", self.session));
                }
                Disposition::Retry => {}
            }
        }
        disposition
    }

    /// A held binding came from an authenticated provision: the budget owns
    /// at least that generation before anything is refused or counted. An
    /// older binding changes nothing, so its outcomes stay stale.
    fn adopt_binding(&mut self) {
        if let Some(binding) = &self.binding {
            self.credential.provisioned(binding.generation);
        }
    }

    /// Report a refusal once, when it first parks the current generation.
    fn parked(&mut self, held: Option<RefusalCause>) {
        if let (None, Some(cause)) = (held, self.credential.refusal()) {
            // A spent floor allowance reports as a capability refusal.
            let failure = match cause {
                RefusalCause::Principal => E::Principal,
                RefusalCause::Floor | RefusalCause::Capability => E::Capability,
            };
            crate::diagnostics::report(format!("native mail rebind required session={} cause={failure:?}; relaunch or re-provision the seat", self.session));
        }
        self.attention = MailAttentionInfo::RebindRequired;
    }

    pub(super) fn requires_rebind(&mut self) -> bool {
        // Adopt the latest authenticated provision, however many came between.
        let provisioned = self
            .custody
            .lock()
            .ok()
            .and_then(|custody| custody.generations.get(&self.session).copied());
        if let Some(generation) = provisioned {
            self.credential.provisioned(generation);
        }
        if self.credential.refusal().is_none() {
            return false;
        }
        // Removing/disabling an identity hides mail, but cannot forgive the
        // refused credential if that identity is subsequently restored.
        self.attention = match self.current_binding() {
            Ok(None) => MailAttentionInfo::Disabled,
            _ => MailAttentionInfo::RebindRequired,
        };
        true
    }

    pub(super) fn blocked_attention(&self) -> MailAttentionInfo {
        if self.credential.refusal().is_some() {
            if self.attention == MailAttentionInfo::Disabled {
                MailAttentionInfo::Disabled
            } else {
                MailAttentionInfo::RebindRequired
            }
        } else {
            // Blocked means no reconnect will clear it: say so at once.
            MailAttentionInfo::Stopped
        }
    }
}

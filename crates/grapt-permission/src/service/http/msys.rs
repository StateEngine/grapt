//! Session approval reuse for Windows command trees.
//! Copyright OpenAI. Apache-2.0; see licenses/openai-codex/
//! Modified: one bounded existing authority, explicit Windows command-tree scope.
use super::*;

impl PermissionHttpState {
    /// Preparation hint only; the final permission decision rechecks this grant
    /// and every ordinary command consequence before dispatch.
    pub fn msys_session_granted(&self, session: &SessionId) -> bool {
        self.session_approvals
            .iter()
            .any(|(held, action, resources)| {
                held == session
                    && action == crate::SANDBOX_MSYS_ACTION
                    && resources
                        .iter()
                        .any(|value| value == crate::SANDBOX_MSYS_SESSION)
            })
    }
}

pub(super) fn check_reply(
    action: &str,
    resources: &[String],
    reply: PermissionHttpReply,
) -> Result<(), PermissionHttpError> {
    if action == crate::SANDBOX_MSYS_ACTION
        && resources
            .iter()
            .any(|value| value == crate::SANDBOX_MSYS_SESSION)
        && reply == PermissionHttpReply::Always
    {
        return Err(PermissionError::InvalidAction {
            action: "MSYS command-tree access is Session-only; choose Once, Session or Reject"
                .into(),
        }
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::{Effect, Layer, Preset, Rule};

    #[test]
    fn command_tree_consent_is_session_only_and_cannot_grant_a_denied_command(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = EventStore::open_in_memory()?;
        PermissionHttpState::initialize(&mut store)?;
        let session: SessionId = "ses_msys_tree".parse()?;
        let other: SessionId = "ses_msys_other".parse()?;
        let project = ProjectId::new("prj_msys_tree").ok_or("project")?;
        let mut state = PermissionHttpState::default();
        let action = crate::SANDBOX_MSYS_ACTION;
        let resource = crate::SANDBOX_MSYS_SESSION;
        let layers = [Layer::preset(Preset::Maintainer)];
        let preview = |state: &PermissionHttpState, session: &SessionId| {
            state.preview_tool(
                &store,
                session,
                &layers,
                &project,
                action,
                &[resource.into()],
                None,
            )
        };
        state.session_approvals.push((
            session.clone(),
            action.into(),
            vec!["C:/Git/bash.exe".into()],
        ));
        assert!(!state.msys_session_granted(&session));
        assert_eq!(preview(&state, &session)?, PermissionCreateEffect::Ask);
        state
            .session_approvals
            .push((session.clone(), action.into(), vec![resource.into()]));
        assert!(state.msys_session_granted(&session));
        assert_eq!(preview(&state, &session)?, PermissionCreateEffect::Allow);
        assert!(!state.msys_session_granted(&other));
        assert_eq!(preview(&state, &other)?, PermissionCreateEffect::Ask);
        let denied = [Layer {
            name: crate::preset::LayerName::Config,
            rules: vec![Rule::new("shell", "*", Effect::Deny)],
            floor: false,
            compose: crate::preset::Compose::Narrow,
        }];
        let resources: [String; 2] = ["powershell.exe -File fetch.ps1".into(), resource.into()];
        let consequences = [
            PermissionConsequence {
                action: "shell".into(),
                resource: resources[0].clone(),
            },
            PermissionConsequence {
                action: action.into(),
                resource: resources[1].clone(),
            },
        ];
        assert!(matches!(
            state.preview_tool(
                &store,
                &session,
                &denied,
                &project,
                action,
                &resources,
                Some(&consequences)
            ),
            Err(PermissionHttpError::Denied { .. })
        ));
        assert!(check_reply(action, &[resource.into()], PermissionHttpReply::Always).is_err());
        assert!(check_reply(action, &[resource.into()], PermissionHttpReply::Session).is_ok());
        Ok(())
    }
}

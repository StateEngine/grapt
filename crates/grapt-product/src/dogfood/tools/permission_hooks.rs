use super::*;
use grapt_permission::preset::Layer;
use grapt_permission::{PermissionConsequence, PermissionCreateEffect};

pub(super) struct PermissionPlan {
    pub(super) consequences: Option<Vec<PermissionConsequence>>,
    pub(super) resources: Vec<String>,
    pub(super) checks: Vec<PermissionConsequence>,
    pub(super) save: Vec<String>,
    pub(super) layers: Vec<Layer>,
    pub(super) metadata: Option<serde_json::Value>,
}

impl PermissionToolRunner {
    pub(super) fn permission_plan(
        &self,
        prepared: &PreparedEffect,
        input: &BoundedToolInput,
        hook: super::super::hooks::PermissionDecision,
    ) -> Result<PermissionPlan, PermissionStartFailure> {
        let msys = match &prepared.target {
            EffectTarget::Shell { environment, .. } => environment.msys_executables(),
            EffectTarget::TerminalExec(spec) => spec.environment.msys_executables(),
            _ => &[],
        };
        let consequences = if msys.is_empty() {
            match &prepared.target {
                EffectTarget::Coding(coding::CodingTarget::Patch(plan)) => {
                    Some(plan.consequences())
                }
                EffectTarget::TerminalExec(spec) => sandbox_access::consequences(spec),
                _ => None,
            }
        } else {
            let mut items = vec![PermissionConsequence {
                action: SHELL_TOOL.into(),
                resource: prepared.resource.clone(),
            }];
            items.push(PermissionConsequence {
                action: grapt_permission::SANDBOX_MSYS_ACTION.into(),
                resource: grapt_permission::SANDBOX_MSYS_SESSION.into(),
            });
            Some(items)
        };
        let resources = consequences.as_ref().map_or_else(
            || vec![prepared.resource.clone()],
            |items| items.iter().map(|item| item.resource.clone()).collect(),
        );
        let checks = consequences.clone().unwrap_or_else(|| {
            vec![grapt_permission::PermissionConsequence {
                action: prepared.action.clone(),
                resource: prepared.resource.clone(),
            }]
        });
        let mut layers = match &prepared.target {
            EffectTarget::TerminalExec(spec) => {
                self.policy
                    .terminal_layers(&checks, &spec.command, terminal_prefix_grants(spec))
            }
            _ => self.policy.layers(None, &checks),
        };
        hook_decision::layers(&mut layers, &checks, hook);
        sandbox_access::consent_layers(&mut layers, &checks);
        let save = if prepared.action == grapt_permission::SANDBOX_WRITE_ACTION {
            checks
                .iter()
                .filter(|check| check.action == grapt_permission::SANDBOX_WRITE_ACTION)
                .map(|check| check.resource.clone())
                .collect()
        } else {
            resources.clone()
        };
        let mut metadata =
            approval_preview::metadata(&self.root, prepared, input, consequences.as_deref());
        if !msys.is_empty() {
            let value = metadata.get_or_insert_with(|| serde_json::json!({}));
            value["sandboxDegradation"] = serde_json::json!({
                "reason": sandbox::msys::REASON,
                "scope": "Once: this command and all its children run without OS containment. Session: all Windows shell commands and their children run without OS filesystem or network restrictions until this Session ends.",
                "sessionOnly": true,
                "executables": msys.iter().map(|path| path_text(path)).collect::<Vec<_>>(),
            });
        }
        Ok(PermissionPlan {
            consequences,
            resources,
            checks,
            save,
            layers,
            metadata,
        })
    }

    pub(super) fn needs_permission_hook(
        &self,
        plan: &PermissionPlan,
        prepared: &PreparedEffect,
    ) -> Result<bool, String> {
        let permissions = lock(&self.permissions);
        let inbox = lock(&self.inbox);
        let preview = permissions.preview_tool(
            inbox.store(),
            &self.session,
            &plan.layers,
            &self.project,
            &prepared.action,
            &plan.resources,
            plan.consequences.as_deref(),
        );
        match preview {
            Ok(effect) => Ok(effect == PermissionCreateEffect::Ask),
            // Keep an existing denial on the ordinary authorization path. It
            // neither invokes an approval hook nor changes the refusal's kind.
            Err(grapt_permission::PermissionHttpError::Denied { .. }) => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    }
}

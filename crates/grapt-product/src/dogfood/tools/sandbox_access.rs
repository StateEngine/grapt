//! Explicit consent for a narrow, contained retry. Stderr is never authority.
//! Apache-2.0, Copyright OpenAI.
use super::*;

pub(super) const TOOL: &str = "request_sandbox_access";
pub(super) const DESCRIPTION: &str = "Request narrow access for a contained command retry: supply exactly one of path (existing absolute file or directory) or host. New files require requesting their parent explicitly. cmd runs only after consent; earlier effects may repeat. Protected paths cannot be granted. Host grants currently return an explicit cannot-enforce refusal. Stderr never authorizes access.";

tool_arguments! {
    pub(super) struct Arguments => TOOL {
        required cmd: NonEmptyString,
        optional description: ToolDescription,
        optional workdir: NonEmptyString,
        optional shell: NonEmptyString,
        optional login: bool,
        optional tty: bool,
        optional yield_time_ms: BoundedU64<250, 30_000>,
        optional timeout_ms: BoundedU64<1, 86_400_000>,
        optional max_output_tokens: BoundedU64<1, 16_384>,
        // Strict provider schemas require every property; null omits the
        // inactive resource without inventing a second grant target.
        nullable path: NonEmptyString,
        nullable host: NonEmptyString,
    }
}

pub(super) fn prepare(
    root: &Path,
    input: &BoundedToolInput,
    environment: shell_environment::Environment,
) -> Result<PreparedEffect, String> {
    // Decode through the strict public schema first, then reuse the ordinary
    // terminal decoder for command, shell, directory and output bounds.
    let arguments: Arguments = decode_input(input)?;
    match (&arguments.path, &arguments.host) {
        (Some(_), None) => {}
        (None, Some(host)) => {
            if host.as_str().len() > 253 || host.as_str().chars().any(char::is_control) {
                return Err(
                    "Sandbox host must contain at most 253 bytes without control characters".into(),
                );
            }
            return Err(format!("Sandbox cannot enforce this host grant: {}. The command was not retried and network access is unchanged", host.as_str()));
        }
        _ => return Err("Request exactly one sandbox resource: path or host".into()),
    }
    let mut value: serde_json::Value =
        serde_json::from_slice(input.as_bytes()).map_err(|error| error.to_string())?;
    let fields = value
        .as_object_mut()
        .ok_or("Sandbox arguments must be an object")?;
    fields.remove("host");
    let requested = fields
        .remove("path")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or("Sandbox requested target is absent")?;
    let plan = environment
        .sandbox()
        .ok_or("Sandbox is not enabled for this command")?;
    let target =
        grapt_sandbox::WriteTarget::resolve(&plan.preview_policy(), Path::new(&requested))?;
    let resource = path_text(target.path());
    if resource.len() > 8192 {
        return Err("Sandbox requested target exceeds the permission resource bound".into());
    }
    let input =
        BoundedToolInput::new(serde_json::to_vec(&value).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let mut prepared =
        terminal::prepare_exec(root, &input, environment.with_write_target(target)?)?;
    prepared.action = grapt_permission::SANDBOX_WRITE_ACTION.into();
    prepared.resource = resource;
    Ok(prepared)
}

pub(super) fn consequences(
    spec: &terminal::ExecSpec,
) -> Option<Vec<grapt_permission::PermissionConsequence>> {
    let targets = spec.environment.write_targets();
    if targets.is_empty() {
        return None;
    }
    let mut checks = targets
        .iter()
        .map(|target| grapt_permission::PermissionConsequence {
            action: grapt_permission::SANDBOX_WRITE_ACTION.into(),
            resource: path_text(target.path()),
        })
        .collect::<Vec<_>>();
    checks.push(grapt_permission::PermissionConsequence {
        action: "shell".into(),
        resource: terminal::permission_resource(spec),
    });
    Some(checks)
}

/// General tool allows and hooks cannot mint a filesystem expansion. Preserve
/// their ordinary command decision, and let only the permission owner's exact
/// saved/session resource approvals satisfy the sandbox consequence.
pub(super) fn consent_layers(
    layers: &mut Vec<grapt_permission::preset::Layer>,
    checks: &[grapt_permission::PermissionConsequence],
) {
    use grapt_permission::preset::{decide, Compose, Effect, Layer, LayerName, Rule};
    if !checks
        .iter()
        .any(|check| check.action == grapt_permission::SANDBOX_WRITE_ACTION)
    {
        return;
    }
    for layer in layers {
        let rules = checks
            .iter()
            .filter_map(|check| {
                let expansion = check.action == grapt_permission::SANDBOX_WRITE_ACTION;
                let effect = if layer.name == LayerName::Bypass {
                    if expansion {
                        return None;
                    }
                    Effect::Allow
                } else {
                    let decision = decide(
                        &check.action,
                        &[&check.resource],
                        std::slice::from_ref(layer),
                    );
                    if decision.layer.is_none() || (expansion && decision.effect == Effect::Allow) {
                        return None;
                    }
                    decision.effect
                };
                Some(Rule::literal(&check.action, &check.resource, effect))
            })
            .collect();
        if layer.name == LayerName::Bypass {
            *layer = Layer {
                name: LayerName::Config,
                rules,
                floor: false,
                compose: Compose::Override,
            };
        } else {
            layer.rules = rules;
        }
    }
}

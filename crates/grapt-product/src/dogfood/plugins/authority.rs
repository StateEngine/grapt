//! The one dispatch chokepoint that turns a proven plugin host context into
//! product authority for a single host operation.
//!
//! The supervisor proves which process sent a request, which plugin that
//! process loaded, and whether the named call grant is live on that process.
//! This module decides what that proof permits: plugin-level reads need only
//! the transport identity; session, credential-writing, web-search, event, and
//! command operations need a live call grant whose scope names the Session.

use grapt_plugin::{ActiveGrant, HostOperation, PluginHostContext};
use serde_json::Value;

/// Product authority carried by a plugin call grant.
#[allow(
    missing_debug_implementations,
    reason = "the scope owns an opaque authenticated principal"
)]
pub(crate) struct PluginCallScope {
    pub(crate) principal: grapt_api::SealedPrincipal,
    pub(crate) session: grapt_types::SessionId,
    pub(crate) agent: grapt_types::AgentId,
    /// The message whose tool call or hook opened this grant; product effects
    /// the plugin requests are attributed to it. Absent for context hooks.
    pub(crate) message: Option<grapt_types::MessageId>,
    pub(crate) operations: &'static [HostOperation],
}

/// Operations a Session-scoped plugin call may perform.
pub(crate) const SESSION_CALL_OPERATIONS: &[HostOperation] = &[
    HostOperation::EventSubscribe,
    HostOperation::IntegrationConnectKey,
    HostOperation::IntegrationOauthConnect,
    HostOperation::IntegrationOauthStatus,
    HostOperation::IntegrationOauthComplete,
    HostOperation::IntegrationOauthCancel,
    HostOperation::IntegrationCommandConnect,
    HostOperation::IntegrationCommandStatus,
    HostOperation::IntegrationCommandCancel,
    HostOperation::SessionCreate,
    HostOperation::SessionGet,
    HostOperation::SessionPrompt,
    HostOperation::SessionGenerate,
    HostOperation::SessionCommand,
    HostOperation::SessionSynthetic,
    HostOperation::SessionInterrupt,
    HostOperation::WebsearchSelectionPut,
    HostOperation::WebsearchQuery,
];

enum Class {
    /// Read-only product catalogs and the plugin's own credentials.
    Plugin,
    /// Needs a live grant; the Session comes from the grant.
    Call,
    /// Needs a live grant whose Session equals the payload `sessionID`.
    Session,
}

const fn class(operation: HostOperation) -> Class {
    match operation {
        HostOperation::NetworkFetch
        | HostOperation::AgentList
        | HostOperation::AgentGet
        | HostOperation::AgentReload
        | HostOperation::CatalogProviderList
        | HostOperation::CatalogProviderGet
        | HostOperation::CatalogModelList
        | HostOperation::CatalogModelDefault
        | HostOperation::CatalogReload
        | HostOperation::CommandList
        | HostOperation::CommandReload
        | HostOperation::IntegrationList
        | HostOperation::IntegrationGet
        | HostOperation::IntegrationReload
        | HostOperation::IntegrationConnectionActive
        | HostOperation::IntegrationConnectionResolve
        | HostOperation::PluginList
        | HostOperation::ReferenceList
        | HostOperation::ReferenceReload
        | HostOperation::SkillList
        | HostOperation::SkillReload
        | HostOperation::WebsearchProviders
        | HostOperation::WebsearchSelectionGet
        | HostOperation::WebsearchReload => Class::Plugin,
        HostOperation::EventSubscribe
        | HostOperation::IntegrationConnectKey
        | HostOperation::IntegrationOauthConnect
        | HostOperation::IntegrationOauthStatus
        | HostOperation::IntegrationOauthComplete
        | HostOperation::IntegrationOauthCancel
        | HostOperation::IntegrationCommandConnect
        | HostOperation::IntegrationCommandStatus
        | HostOperation::IntegrationCommandCancel
        | HostOperation::SessionCreate
        | HostOperation::WebsearchSelectionPut
        | HostOperation::WebsearchQuery => Class::Call,
        HostOperation::SessionGet
        | HostOperation::SessionPrompt
        | HostOperation::SessionGenerate
        | HostOperation::SessionCommand
        | HostOperation::SessionSynthetic
        | HostOperation::SessionInterrupt => Class::Session,
    }
}

/// Authority established for one host operation.
pub(super) struct Authorized<'a> {
    /// The admitted declaration identity that owns durable plugin state.
    pub(super) owner: &'a str,
    pub(super) process: u64,
    pub(super) call: Option<AuthorizedCall<'a>>,
}

pub(super) struct AuthorizedCall<'a> {
    pub(super) grant: &'a ActiveGrant,
    pub(super) scope: &'a PluginCallScope,
}

impl<'a> Authorized<'a> {
    pub(super) fn call(&self) -> Result<&AuthorizedCall<'a>, String> {
        self.call
            .as_ref()
            .ok_or_else(|| "plugin operation requires an active plugin call".to_owned())
    }
}

pub(super) fn authorize<'a>(
    context: &'a PluginHostContext,
    operation: HostOperation,
    payload: &Value,
) -> Result<Authorized<'a>, String> {
    let plugin = context.plugin();
    let call = match class(operation) {
        Class::Plugin => None,
        Class::Call | Class::Session => {
            let grant = context.grant().ok_or_else(|| {
                format!(
                    "plugin {plugin} {operation:?} requires an active plugin call; Session authority is not available outside a call"
                )
            })?;
            let scope = grant
                .scope()
                .downcast_ref::<PluginCallScope>()
                .ok_or_else(|| "plugin call grant carries an unrecognized scope".to_owned())?;
            if !scope.operations.contains(&operation) {
                return Err(format!(
                    "plugin {plugin} call grant does not permit {operation:?}"
                ));
            }
            if matches!(class(operation), Class::Session) {
                require_granted_session(payload, operation, &scope.session)?;
            }
            Some(AuthorizedCall { grant, scope })
        }
    };
    Ok(Authorized {
        owner: context.owner(),
        process: context.process(),
        call,
    })
}

/// A Session-bound operation may name only the Session its grant was issued for.
fn require_granted_session(
    payload: &Value,
    operation: HostOperation,
    granted: &grapt_types::SessionId,
) -> Result<(), String> {
    if session_id(payload, operation)? != *granted {
        return Err("plugin call authority belongs to another Session".to_owned());
    }
    Ok(())
}

pub(super) fn session_id(
    payload: &Value,
    operation: HostOperation,
) -> Result<grapt_types::SessionId, String> {
    payload
        .get("sessionID")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{operation:?} requires sessionID"))?
        .parse::<grapt_types::SessionId>()
        .map_err(|_| format!("{operation:?} sessionID is invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_bound_operation_refuses_every_session_but_its_grant() {
        let granted = "ses_granted"
            .parse::<grapt_types::SessionId>()
            .unwrap_or_else(|_| panic!("literal session id was refused"));
        assert_eq!(
            require_granted_session(
                &serde_json::json!({"sessionID":"ses_granted"}),
                HostOperation::SessionInterrupt,
                &granted
            ),
            Ok(())
        );
        for operation in [
            HostOperation::SessionInterrupt,
            HostOperation::SessionPrompt,
            HostOperation::SessionGet,
        ] {
            assert_eq!(
                require_granted_session(
                    &serde_json::json!({"sessionID":"ses_foreign"}),
                    operation,
                    &granted
                ),
                Err("plugin call authority belongs to another Session".to_owned()),
                "{operation:?} reached a foreign Session"
            );
        }
        assert!(matches!(
            class(HostOperation::SessionInterrupt),
            Class::Session
        ));
    }
}

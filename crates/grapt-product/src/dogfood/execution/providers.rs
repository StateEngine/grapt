//! One existing attempt opener per configured provider account, fixed at startup.
use super::*;

#[cfg(test)]
#[path = "providers_tests.rs"]
mod tests;

#[allow(clippy::too_many_arguments)]
pub(crate) fn open(
    targets: &ProviderTargetAuthority,
    primary: CredentialAuthority,
    secondary: Vec<(ProviderId, CredentialAuthority)>,
    endpoint: &LiveEndpoint,
    limits: ProductionProviderLimits,
    dispatch: grapt_provider::DispatchObserver,
    error_text: grapt_provider::ProviderErrorText,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
) -> Result<
    (
        ResolvedModelTarget,
        BTreeMap<ProviderId, Arc<AttemptOpener>>,
    ),
    ExecutionRefusal,
> {
    open_mode(
        targets, primary, secondary, endpoint, limits, dispatch, error_text, clock, false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn open_mode(
    targets: &ProviderTargetAuthority,
    primary: CredentialAuthority,
    secondary: Vec<(ProviderId, CredentialAuthority)>,
    endpoint: &LiveEndpoint,
    limits: ProductionProviderLimits,
    dispatch: grapt_provider::DispatchObserver,
    error_text: grapt_provider::ProviderErrorText,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    ephemeral: bool,
) -> Result<
    (
        ResolvedModelTarget,
        BTreeMap<ProviderId, Arc<AttemptOpener>>,
    ),
    ExecutionRefusal,
> {
    if secondary.len() > 7 {
        return Err(ExecutionRefusal::Compose(
            "provider account bound exceeded".into(),
        ));
    }
    let primary_target = targets
        .issue()
        .map_err(|error| ExecutionRefusal::Compose(error.to_string()))?;
    let mut openers = BTreeMap::new();
    openers.insert(
        primary_target.provider().clone(),
        Arc::new(opener(
            targets,
            &primary_target,
            primary,
            endpoint,
            limits,
            dispatch.clone(),
            error_text.clone(),
            Arc::clone(&clock),
            ephemeral,
        )?),
    );
    for (provider, credential) in secondary {
        if openers.contains_key(&provider) {
            return Err(ExecutionRefusal::Compose(
                "duplicate provider custody".into(),
            ));
        }
        let inventory = targets.inventory().ok_or_else(|| {
            ExecutionRefusal::Compose("secondary provider inventory absent".into())
        })?;
        let model = inventory
            .available()
            .iter()
            .find(|model| model.provider() == &provider)
            .ok_or_else(|| {
                ExecutionRefusal::Compose("secondary provider has no issued model".into())
            })?;
        let target = targets
            .issue_selected(&provider, model.id(), None)
            .map_err(|error| ExecutionRefusal::Compose(error.to_string()))?;
        openers.insert(
            provider,
            Arc::new(opener(
                targets,
                &target,
                credential,
                endpoint,
                limits,
                dispatch.clone(),
                error_text.clone(),
                Arc::clone(&clock),
                ephemeral,
            )?),
        );
    }
    Ok((primary_target, openers))
}

#[allow(clippy::too_many_arguments)]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn opener(
    targets: &ProviderTargetAuthority,
    target: &ResolvedModelTarget,
    credential: CredentialAuthority,
    endpoint: &LiveEndpoint,
    limits: ProductionProviderLimits,
    dispatch: grapt_provider::DispatchObserver,
    error_text: grapt_provider::ProviderErrorText,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    ephemeral: bool,
) -> Result<AttemptOpener, ExecutionRefusal> {
    let failure = |error: String| ExecutionRefusal::Compose(error);
    let family = targets
        .protocol_family_for(target.provider())
        .ok_or_else(|| failure("provider protocol absent".into()))?;
    let compatible_protocol = targets
        .compatible_protocol_for(target.provider())
        .ok_or_else(|| failure("provider compatible format absent".into()))?;
    let request = CredentialResolutionRequest::new(
        target,
        credential.record().clone(),
        family,
        credential.placement(),
    );
    // A fixed capability has no sign-in lock, refresh task or profile writer.
    // Expiry/rejection is a provider refusal, never an implicit persistent renewal.
    let (credential, owner) = if ephemeral || credential.is_vaulted_api_key() {
        (
            credential
                .resolve(request)
                .map_err(|error| failure(error.to_string()))?,
            None,
        )
    } else {
        let (credential, reloadable) = credential
            .into_refreshable(request)
            .map_err(|error| failure(error.to_string()))?;
        let owner = super::credentials::LiveCredentials::open(reloadable, family, clock)
            .map_err(failure)?;
        (credential, Some(owner))
    };
    let deadlines = CodexDeadlines::new(limits.connect, limits.response_head)
        .map_err(|error| failure(error.to_string()))?;
    let budget = CodexLimits::new(limits.active_attempts);
    let result = match endpoint {
        LiveEndpoint::Production(endpoint) => {
            let configured = match targets.endpoint_for(target.provider()) {
                Some(base) => endpoint.clone().with_configured_profile(base),
                None => endpoint.clone(),
            };
            let configured = match targets.cloudflare_for(target.provider()) {
                Some(scope) => configured.with_cloudflare(scope.clone()),
                None => configured,
            };
            AttemptOpener::route(
                family,
                deadlines,
                budget,
                configured.with_messages_auth(targets.messages_auth_for(target.provider())),
                target.route().clone(),
                concat!("grapt-product/", env!("CARGO_PKG_VERSION")),
                credential,
            )
        }
        #[cfg(feature = "loopback-provider-testkit")]
        LiveEndpoint::Loopback(base) if targets.cloudflare_for(target.provider()).is_some() => {
            let scope = targets
                .cloudflare_for(target.provider())
                .ok_or_else(|| failure("Cloudflare scope is absent".into()))?;
            let fixture = grapt_login::key::cloudflare::fixture_base(scope, base.as_str())
                .map_err(|_| failure("invalid Cloudflare fixture origin".into()))?;
            AttemptOpener::route(
                family,
                deadlines,
                budget,
                EndpointConfig::new()
                    .with_attempt_override(fixture)
                    .with_cloudflare(scope.clone()),
                target.route().clone(),
                "grapt-loopback",
                credential,
            )
        }
        #[cfg(feature = "loopback-provider-testkit")]
        LiveEndpoint::Loopback(base)
            if targets.messages_auth_for(target.provider())
                == grapt_provider::MessagesAuthMode::Bearer =>
        {
            AttemptOpener::route(
                family,
                deadlines,
                budget,
                EndpointConfig::new()
                    .with_attempt_override(base.as_str())
                    .with_messages_auth(grapt_provider::MessagesAuthMode::Bearer),
                target.route().clone(),
                "grapt-loopback",
                credential,
            )
        }
        #[cfg(feature = "loopback-provider-testkit")]
        LiveEndpoint::Loopback(base) => AttemptOpener::loopback_testkit(
            family,
            deadlines,
            budget,
            base.clone(),
            target.route().clone(),
            credential,
        ),
    };
    result
        .and_then(|opener| {
            if family == grapt_provider::ProtocolFamily::WatsonxChat {
                let project = targets
                    .watsonx_project_for(target.provider())
                    .cloned()
                    .ok_or(grapt_provider::AttemptOpenRefusal::Route(
                        grapt_provider::RouteConfigError::WatsonxProjectRequired,
                    ))?;
                opener.with_watsonx_project(project)
            } else {
                Ok(opener)
            }
        })
        .map(|opener| {
            let opener = opener
                .with_compatible_protocol(compatible_protocol)
                .with_child_capacity(limits.child_attempts)
                .with_dispatch_observer(dispatch)
                .with_error_text(error_text);
            match owner {
                Some(owner) => opener.with_credential_owner(owner),
                None => opener,
            }
        })
        .map_err(|error| failure(error.to_string()))
}

impl LiveLocation {
    pub(super) fn provider_attempts<P: AttemptPlan>(
        &self,
        opener: &Arc<AttemptOpener>,
        session: &SessionId,
        record: &grapt_session::SessionRecord,
        plan: P,
        retry_feedback: crate::dogfood::session_status::RetryFeedback,
        execution: &ExecutionId,
    ) -> Result<ProviderAttemptSource<P>, String> {
        let mut attempts = ProviderAttemptSource::new(
            Arc::clone(opener),
            Arc::clone(&self.inbox),
            Arc::clone(&self.ids) as Arc<dyn IdAuthority + Send + Sync>,
            Arc::clone(&self.runtime_clock) as Arc<dyn Clock>,
            session.clone(),
            self.ceiling,
            plan,
        )
        .with_capacity_observer(Arc::new(move |waiting| {
            retry_feedback.provider_slot(waiting)
        }));
        if !self.ephemeral {
            attempts =
                attempts.with_error_observer(Arc::new(crate::provider_error_text::Observer {
                    root: self.service_root.clone(),
                    session: session.clone(),
                    execution: execution.clone(),
                    inbox: self.inbox.clone(),
                    ids: self.ids.clone(),
                    clock: self.clock.clone(),
                }));
        }
        if record.parent.is_some() {
            let limit = crate::dogfood::provider_capacity::effective(
                &self.service_root,
                Path::new(record.placement.directory()),
            )?;
            attempts = attempts.with_child_capacity(limit);
        }
        Ok(attempts)
    }
}

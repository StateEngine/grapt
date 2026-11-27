//! Select volatile owners before opening any execution-owned persistence.
use super::*;
use crate::dogfood::session_status::{install_hydration, install_volatile_hydration};
#[cfg(feature = "loopback-provider-testkit")]
use std::time::Duration;

/// This crate's fixture policy identity, distinct from the ruled default: the
/// base backoff differs, so it must not borrow the ruled-default version.
#[cfg(feature = "loopback-provider-testkit")]
const FIXTURE_POLICY_VERSION: &str = "secode-product/step-policy/testkit-backoff/1";

/// The Step policy this process runs under.
///
/// Ordinary builds always run [`StepPolicy::ruled_default`]. The testkit build
/// may narrow ONLY the base backoff through `GRAPT_RETRY_BASE_MS`, like
/// `GRAPT_SUBAGENT_BUDGET_MS`: the ruled 2+4+8+16 s schedule costs a gate run
/// about 30 s per exhaustion case. Every other bound comes from the ruled
/// default, so this cannot widen an attempt count, a rebuild or a timeout.
fn step_policy() -> Result<StepPolicy, String> {
    #[cfg(feature = "loopback-provider-testkit")]
    if let Some(raw) = std::env::var_os("GRAPT_RETRY_BASE_MS") {
        let text = raw
            .to_str()
            .ok_or_else(|| "GRAPT_RETRY_BASE_MS is not valid UTF-8".to_owned())?;
        let millis = text.parse::<u64>().map_err(|error| {
            format!("GRAPT_RETRY_BASE_MS {text:?} is not a millisecond count: {error}")
        })?;
        let ruled =
            StepPolicy::ruled_default().map_err(|error| format!("ruled policy: {error}"))?;
        let version = grapt_types::PolicyVersion::new(FIXTURE_POLICY_VERSION)
            .ok_or("the fixture policy version is empty")?;
        // The ruled policy's OWN bounds are carried across, so only the base
        // backoff can differ and nothing here can widen a production bound.
        return StepPolicy::new_with_max_retry_delay(
            version,
            ruled.max_attempts(),
            Duration::from_millis(millis).min(ruled.base_backoff()),
            ruled.max_retry_delay(),
            ruled.max_overflow_rebuilds(),
            ruled.no_progress_timeout(),
        )
        .map_err(|error| format!("fixture policy: {error}"));
    }
    StepPolicy::ruled_default().map_err(|error| format!("ruled policy: {error}"))
}

impl LiveLocation {
    pub(super) fn configure_tools(
        &self,
        runner: PermissionToolRunner,
        feedback: crate::dogfood::session_status::RetryFeedback,
        hooks: Arc<super::super::hooks::HookRuntime>,
    ) -> PermissionToolRunner {
        let runner = runner
            .with_ephemeral(self.ephemeral)
            .with_status_feedback(feedback)
            .with_shadow_ledger(Arc::clone(&self.shadow));
        if self.ephemeral {
            runner
        } else {
            runner
                .with_checkpoints(self.service_root.clone())
                .with_hooks(hooks)
        }
    }
}

impl LiveExecutor {
    pub fn open(
        service_root: &Path,
        dependencies: LiveDependencies,
        targets: ProviderTargetAuthority,
        credential: CredentialAuthority,
        secondary_credentials: Vec<(ProviderId, CredentialAuthority)>,
        endpoint: LiveEndpoint,
        limits: ProductionProviderLimits,
    ) -> Result<Self, ExecutionRefusal> {
        Self::open_mode(
            service_root,
            dependencies,
            targets,
            credential,
            secondary_credentials,
            endpoint,
            limits,
            false,
        )
    }

    /// Execution state and artifacts stay in memory. The caller must also
    /// supply an in-memory Inbox and store-free application dependencies.
    pub fn open_ephemeral(
        service_root: &Path,
        dependencies: LiveDependencies,
        targets: ProviderTargetAuthority,
        credential: CredentialAuthority,
        secondary_credentials: Vec<(ProviderId, CredentialAuthority)>,
        endpoint: LiveEndpoint,
        limits: ProductionProviderLimits,
    ) -> Result<Self, ExecutionRefusal> {
        Self::open_mode(
            service_root,
            dependencies,
            targets,
            credential,
            secondary_credentials,
            endpoint,
            limits,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn open_mode(
        service_root: &Path,
        dependencies: LiveDependencies,
        targets: ProviderTargetAuthority,
        credential: CredentialAuthority,
        secondary_credentials: Vec<(ProviderId, CredentialAuthority)>,
        endpoint: LiveEndpoint,
        limits: ProductionProviderLimits,
        ephemeral: bool,
    ) -> Result<Self, ExecutionRefusal> {
        let targets = PlatformTargets(targets);
        targets
            .require_context()
            .map_err(ExecutionRefusal::Compose)?;
        let LiveDependencies {
            catalog,
            wake_slots,
            inbox,
            ids,
            clock,
            hub,
            status,
            permissions,
            tool_permissions,
            session_capabilities,
            native_mail,
            mcp,
            plugins,
            child_seam,
        } = dependencies;
        let continuity = super::super::continuity_attention::ContinuityAttention::new(
            Arc::clone(&ids) as Arc<dyn grapt_types::IdAuthority + Send + Sync>,
            Arc::clone(&clock) as Arc<dyn grapt_types::ClockAuthority + Send + Sync>,
            Arc::clone(&hub),
        );
        if let Ok(mut custody) = session_capabilities.lock() {
            custody.attention = Some(Arc::clone(&continuity));
        }
        if !ephemeral {
            continuity.seed_rebind(&inbox, &session_capabilities);
        }
        if ephemeral {
            Self::require_memory_inbox(&inbox).map_err(ExecutionRefusal::Compose)?;
        }
        let receipt_bytes = NonZeroU32::new(super::super::ATTEMPT_RECEIPT_BYTES)
            .ok_or_else(|| ExecutionRefusal::Compose("attempt receipt bound is zero".to_owned()))?;
        let ceiling = AttemptRecordCeiling::new(receipt_bytes);
        let (dispatch, dispatch_samples) = grapt_provider::DispatchObserver::bounded(Arc::clone(
            &clock,
        )
            as Arc<dyn ClockAuthority + Send + Sync>);
        let error_text = if ephemeral {
            grapt_provider::ProviderErrorText::disabled()
        } else {
            crate::provider_error_text::open(service_root).map_err(ExecutionRefusal::Compose)?
        };
        let (credential_target, openers) = if ephemeral {
            providers::open_mode(
                &targets.0,
                credential,
                secondary_credentials,
                &endpoint,
                limits,
                dispatch.clone(),
                error_text.clone(),
                Arc::clone(&clock) as Arc<dyn ClockAuthority + Send + Sync>,
                true,
            )
        } else {
            providers::open(
                &targets.0,
                credential,
                secondary_credentials,
                &endpoint,
                limits,
                dispatch.clone(),
                error_text.clone(),
                Arc::clone(&clock) as Arc<dyn ClockAuthority + Send + Sync>,
            )
        }?;
        let (timing, tail_samples) = if ephemeral {
            timing::TailObserver::volatile(
                Arc::clone(&clock) as Arc<dyn ClockAuthority + Send + Sync>
            )
        } else {
            timing::TailObserver::bounded(
                Arc::clone(&clock) as Arc<dyn ClockAuthority + Send + Sync>
            )
        };
        let timing: Arc<dyn grapt_types::timing::TimingObserver> = timing;
        match inbox.lock() {
            Ok(mut inbox) => inbox
                .store_mut()
                .install_timing_observer(Arc::clone(&timing)),
            Err(poisoned) => poisoned
                .into_inner()
                .store_mut()
                .install_timing_observer(Arc::clone(&timing)),
        }
        let runtime_clock = Arc::new(RuntimeClock(Arc::clone(&clock)));
        let targets = Arc::new(targets);
        let providers = super::super::provider_generation::Publication::new(
            catalog,
            Some(Arc::new(LiveProviders {
                configured_provider: credential_target.provider().clone(),
                configured_model: credential_target.model().clone(),
                targets,
                first_target: Mutex::new(Some(credential_target)),
                openers,
            })),
        );
        let publication = Arc::new(PublicationLedger::new());
        let principals = Arc::new(PromptPrincipals::default());
        let artifacts = Arc::new(if ephemeral {
            FilesystemArtifactStore::in_memory(
                super::super::PRODUCTION_ARTIFACT_KEY.to_vec(),
                Box::new(SystemClock),
            )
        } else {
            open_artifacts(service_root)?
        });
        let published = PublishedSnapshots::default();
        let active = Arc::new(LiveActiveExecutions::with_status(Arc::clone(&status)));
        let location = Arc::new(LiveLocation {
            ephemeral,
            active: Arc::clone(&active),
            session_ends: Arc::default(),
            service_root: service_root.to_path_buf(),
            providers,
            inbox: Arc::clone(&inbox),
            ids: Arc::clone(&ids),
            clock: Arc::clone(&clock),
            runtime_clock,
            timing,
            ceiling,
            hub: Arc::clone(&hub),
            publication: Arc::clone(&publication),
            permissions,
            tool_permissions,
            principals: Arc::clone(&principals),
            child_seam,
            tool_artifacts: Arc::clone(&artifacts),
            codemode_workers: Arc::new(super::super::tools::WorkerRegistry::default()),
            shadow: Arc::new(if ephemeral {
                super::super::tools::shadow::Ledger::disabled()
            } else {
                super::super::tools::shadow::Ledger::open(service_root, &*ids)
            }),
            terminals: Arc::new(super::super::tools::TerminalRegistry::with_history(
                Arc::clone(&inbox),
                Arc::clone(&ids),
                Arc::clone(&clock),
            )),
            published: published.clone(),
            calibrations: super::super::continuation::admission::SessionCalibrations::default(),
            compaction_recovery: super::super::continuation::compaction_recovery::Registry::default(
            ),
            locations: super::super::locations::SessionLocations::default(),
            mcp: Arc::clone(&mcp),
            plugins: Arc::clone(&plugins),
            session_capabilities,
            native_mail: Arc::clone(&native_mail),
        });
        let policy = step_policy().map_err(ExecutionRefusal::Compose)?;
        let application = RuntimeApplication::new_authority_adapted(
            Arc::clone(&inbox),
            Arc::new(OneLocation(Arc::clone(&location))),
            Arc::new(RuntimeEventIds(Arc::clone(&ids))),
            Arc::new(RuntimeClock(Arc::clone(&clock))),
            (policy, coding_step_allowance()),
        );
        if ephemeral {
            install_volatile_hydration(Arc::clone(&active), status, &hub);
        } else {
            install_hydration(
                Arc::clone(&active),
                status,
                (Arc::clone(&native_mail), continuity),
                &hub,
            );
        }
        Ok(Self {
            dispatch,
            error_text,
            coordinator: SessionCoordinator::new(Arc::new(LiveApplication {
                runtime: application,
                principals: Arc::clone(&principals),
                inbox: Arc::clone(&inbox),
            })),
            artifacts,
            inbox,
            ids,
            clock,
            hub,
            publication,
            interrupted: InterruptTruth::default(),
            principals,
            active,
            wake_slots,
            mcp,
            plugins,
            published,
            dispatch_samples: Mutex::new(dispatch_samples),
            tail_samples: Mutex::new(tail_samples),
            provider_errors: if ephemeral {
                super::super::provider_errors::ProviderErrors::disabled()
            } else {
                super::super::provider_errors::ProviderErrors::open(service_root)
            },
            location,
        })
    }
}

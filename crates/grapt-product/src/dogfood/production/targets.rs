//! Platform-owned target selection and read-only generation policy.
use super::{thinking, ProductTargetAuthority};
use grapt_provider::ProtocolFamily;
use grapt_types::ResolvedModelTarget;
use std::sync::Arc;

/// The reasoning a selected model's account catalog row asks for.
fn codex_reasoning(row: Option<&grapt_platform::ConnectedModel>) -> grapt_provider::CodexReasoning {
    row.map_or_else(grapt_provider::CodexReasoning::default, |row| {
        grapt_provider::CodexReasoning {
            effort: effort_policy(Some(row), ProtocolFamily::ChatGptCodex),
            all_turns: row.responses_lite(),
            summary: row.summary_parameter(),
        }
    })
}

fn effort_policy(
    row: Option<&grapt_platform::ConnectedModel>,
    family: ProtocolFamily,
) -> grapt_provider::EffortPolicy {
    let Some(row) = row else {
        return Default::default();
    };
    let updates = match family {
        ProtocolFamily::ChatGptCodex => row.effort_updates(),
        ProtocolFamily::AnthropicMessages | ProtocolFamily::VertexAnthropic => {
            // Discovery supplies choices; the documented protocol gate controls placement.
            grapt_provider::supports_anthropic_effort_updates(row.provider(), row.id())
                && !row.variants().is_empty()
        }
        _ => false,
    };
    let mode = if updates {
        grapt_provider::EffortMode::Chronological
    } else if matches!(
        family,
        ProtocolFamily::AnthropicMessages
            | ProtocolFamily::VertexAnthropic
            | ProtocolFamily::ChatGptCodex
            | ProtocolFamily::WatsonxChat
    ) && row
        .variants()
        .iter()
        .any(|value| grapt_provider::ReasoningEffort::parse(value.as_str()).is_some())
    {
        grapt_provider::EffortMode::Request
    } else {
        grapt_provider::EffortMode::Unavailable
    };
    grapt_provider::EffortPolicy {
        mode,
        default: row
            .default_variant()
            .and_then(|value| grapt_provider::ReasoningEffort::parse(value.as_str())),
    }
}

#[allow(
    missing_debug_implementations,
    reason = "the platform target authority owns the live model registry"
)]
pub(crate) struct PlatformTargets(pub(crate) grapt_platform::ProviderTargetAuthority);

fn missing_context(provider: &str, model: &str) -> String {
    if provider == "custom" || grapt_connections::catalog::endpoint_provider(provider).is_some() {
        // An endpoint reports a model's window only while that model is
        // loaded; connecting it by name loads it and reads the window again.
        let base = if provider == "custom" {
            " --base-url <url>"
        } else {
            ""
        };
        return format!(
            "the server reported no context window for {provider}/{model} when it was connected; connect it by name with grapt login {provider}{base} --model {model}, adding --context-window <tokens> if the server still reports none"
        );
    }
    format!(
        "model inventory is missing a positive context limit for {provider}/{model}; provide model-catalog.json metadata or an account-models.json row with max_model_len before starting the provider"
    )
}

struct SelectedTargets {
    platform: Arc<PlatformTargets>,
    provider: grapt_types::ProviderId,
    protocol_family: ProtocolFamily,
    model: grapt_types::ModelId,
    variant: Option<grapt_types::ModelVariantId>,
}

impl ProductTargetAuthority for SelectedTargets {
    fn compatible_reasoning_replay(&self) -> grapt_provider::CompatibleReasoningReplay {
        self.platform
            .0
            .compatible_reasoning_replay_for(&self.provider)
    }
    fn request_encoding(&self) -> grapt_provider::RequestEncoding {
        encoding(&self.platform.0, &self.provider)
    }
    fn image_input_supported(&self) -> bool {
        self.platform.0.inventory().is_some_and(|inventory| {
            inventory.available().iter().any(|row| {
                row.provider() == &self.provider && row.id() == &self.model && row.supports_images()
            })
        })
    }
    fn original_image_detail_supported(&self) -> bool {
        self.platform.0.inventory().is_some_and(|inventory| {
            inventory.available().iter().any(|row| {
                row.provider() == &self.provider
                    && row.id() == &self.model
                    && row.original_image_detail()
            })
        })
    }
    fn context_tokens(&self) -> Option<u64> {
        self.platform
            .0
            .inventory()?
            .available()
            .iter()
            .find(|row| row.provider() == &self.provider && row.id() == &self.model)
            .and_then(|row| row.context())
    }

    fn anthropic_max_tokens(&self) -> Result<grapt_provider::MaxOutputTokens, String> {
        let inventory = self.platform.0.inventory();
        thinking::output_limit(inventory.as_ref().and_then(|inventory| {
            inventory
                .available()
                .iter()
                .find(|row| row.provider() == &self.provider && row.id() == &self.model)
        }))
    }

    fn output_bound(&self, requested: u32) -> Result<grapt_provider::MaxOutputTokens, String> {
        let inventory = self.platform.0.inventory();
        thinking::output_bound(
            inventory.as_ref().and_then(|inventory| {
                inventory
                    .available()
                    .iter()
                    .find(|row| row.provider() == &self.provider && row.id() == &self.model)
            }),
            requested,
        )
    }

    fn anthropic_thinking(&self) -> Option<grapt_provider::ThinkingConfig> {
        thinking::configuration(
            self.platform
                .0
                .inventory()?
                .available()
                .iter()
                .find(|row| row.provider() == &self.provider && row.id() == &self.model),
        )
    }

    fn effort_policy(&self) -> grapt_provider::EffortPolicy {
        self.platform
            .0
            .inventory()
            .map_or_else(Default::default, |inventory| {
                effort_policy(
                    inventory
                        .available()
                        .iter()
                        .find(|row| row.provider() == &self.provider && row.id() == &self.model),
                    self.protocol_family,
                )
            })
    }
    fn codex_reasoning(&self) -> grapt_provider::CodexReasoning {
        self.platform
            .0
            .inventory()
            .map_or_else(Default::default, |inventory| {
                codex_reasoning(
                    inventory
                        .available()
                        .iter()
                        .find(|row| row.provider() == &self.provider && row.id() == &self.model),
                )
            })
    }

    /// The family of the connection this selection actually resolved to, not
    /// the default connection's. A second provider reached over its own
    /// protocol must not be spoken to in the primary account's dialect.
    fn protocol_family(&self) -> ProtocolFamily {
        self.protocol_family
    }
    fn issue(&self) -> Result<ResolvedModelTarget, String> {
        self.platform
            .0
            .issue_selected(&self.provider, &self.model, self.variant.as_ref())
            .map_err(|error| error.to_string())
    }
}

impl PlatformTargets {
    pub(crate) fn require_context(&self) -> Result<(), String> {
        if self.context_tokens().is_some() {
            return Ok(());
        }
        Err(missing_context(
            self.0.provider().as_str(),
            self.0.model().as_str(),
        ))
    }

    /// A Step chain captures its selection once. A later picker change cannot
    /// redirect a retry or a tool continuation already in progress.
    pub(in crate::dogfood) fn selected(
        self: &Arc<Self>,
        selection: &serde_json::Value,
    ) -> Result<Arc<dyn ProductTargetAuthority>, String> {
        let provider = selection
            .get("providerID")
            .and_then(serde_json::Value::as_str)
            .and_then(grapt_types::ProviderId::new)
            .ok_or("selected provider identity is invalid")?;
        // An unconfigured provider is refused by name. It is never served by
        // the default connection: that substitution would be a silent
        // cross-account swap, and it would also make "unconfigured" look
        // identical to "configured" at the one place it still matters.
        let protocol_family = self
            .0
            .protocol_family_for(&provider)
            .ok_or("selected provider has no matching connection authority")?;
        let model = selection
            .get("id")
            .and_then(serde_json::Value::as_str)
            .and_then(grapt_types::ModelId::new)
            .ok_or("selected model identity is invalid")?;
        let variant = selection
            .get("variant")
            .and_then(serde_json::Value::as_str)
            .filter(|value| *value != "default")
            .map(|value| {
                grapt_types::ModelVariantId::new(value)
                    .ok_or("selected variant identity is invalid")
            })
            .transpose()?;
        let selected = SelectedTargets {
            platform: Arc::clone(self),
            provider,
            protocol_family,
            model,
            variant,
        };
        if selected.context_tokens().is_none() {
            return Err(missing_context(
                selected.provider.as_str(),
                selected.model.as_str(),
            ));
        }
        Ok(Arc::new(selected))
    }
}

impl ProductTargetAuthority for PlatformTargets {
    fn compatible_reasoning_replay(&self) -> grapt_provider::CompatibleReasoningReplay {
        self.0.compatible_reasoning_replay_for(self.0.provider())
    }
    fn request_encoding(&self) -> grapt_provider::RequestEncoding {
        encoding(&self.0, self.0.provider())
    }
    fn image_input_supported(&self) -> bool {
        self.0.inventory().is_some_and(|inventory| {
            inventory.available().iter().any(|row| {
                row.provider() == self.0.provider()
                    && row.id() == self.0.model()
                    && row.supports_images()
            })
        })
    }
    fn original_image_detail_supported(&self) -> bool {
        self.0.inventory().is_some_and(|inventory| {
            inventory.available().iter().any(|row| {
                row.provider() == self.0.provider()
                    && row.id() == self.0.model()
                    && row.original_image_detail()
            })
        })
    }
    fn context_tokens(&self) -> Option<u64> {
        self.0
            .inventory()?
            .available()
            .iter()
            .find(|row| row.provider() == self.0.provider() && row.id() == self.0.model())
            .and_then(|row| row.context())
    }

    fn anthropic_max_tokens(&self) -> Result<grapt_provider::MaxOutputTokens, String> {
        let inventory = self.0.inventory();
        thinking::output_limit(inventory.as_ref().and_then(|inventory| {
            inventory
                .available()
                .iter()
                .find(|row| row.provider() == self.0.provider() && row.id() == self.0.model())
        }))
    }

    fn output_bound(&self, requested: u32) -> Result<grapt_provider::MaxOutputTokens, String> {
        let inventory = self.0.inventory();
        thinking::output_bound(
            inventory.as_ref().and_then(|inventory| {
                inventory
                    .available()
                    .iter()
                    .find(|row| row.provider() == self.0.provider() && row.id() == self.0.model())
            }),
            requested,
        )
    }

    fn anthropic_thinking(&self) -> Option<grapt_provider::ThinkingConfig> {
        thinking::configuration(
            self.0
                .inventory()?
                .available()
                .iter()
                .find(|row| row.provider() == self.0.provider() && row.id() == self.0.model()),
        )
    }

    fn effort_policy(&self) -> grapt_provider::EffortPolicy {
        self.0
            .inventory()
            .map_or_else(Default::default, |inventory| {
                effort_policy(
                    inventory.available().iter().find(|row| {
                        row.provider() == self.0.provider() && row.id() == self.0.model()
                    }),
                    self.0.protocol_family(),
                )
            })
    }
    fn codex_reasoning(&self) -> grapt_provider::CodexReasoning {
        self.0
            .inventory()
            .map_or_else(Default::default, |inventory| {
                codex_reasoning(
                    inventory.available().iter().find(|row| {
                        row.provider() == self.0.provider() && row.id() == self.0.model()
                    }),
                )
            })
    }

    fn protocol_family(&self) -> ProtocolFamily {
        self.0.protocol_family()
    }

    fn issue(&self) -> Result<ResolvedModelTarget, String> {
        self.0.issue().map_err(|error| error.to_string())
    }
}

fn encoding(
    targets: &grapt_platform::ProviderTargetAuthority,
    provider: &grapt_types::ProviderId,
) -> grapt_provider::RequestEncoding {
    grapt_provider::RequestEncoding::new(
        targets
            .compatible_protocol_for(provider)
            .unwrap_or(grapt_provider::CompatibleProtocol::OpenAi),
        targets.cloudflare_for(provider).cloned(),
        targets.watsonx_project_for(provider).cloned(),
    )
}

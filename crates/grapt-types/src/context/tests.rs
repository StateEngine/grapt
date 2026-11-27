    use super::*;
    use crate::artifact::{ArtifactDigest, ArtifactId};
    use crate::model::{ModelId, ProviderId};

    fn artifact(name: &str) -> Option<ArtifactRef> {
        Some(ArtifactRef::new(
            ArtifactId::new(name)?,
            12,
            ArtifactDigest::new("digest")?,
        ))
    }

    fn model(provider: &str, model: &str) -> Option<ModelRef> {
        Some(ModelRef::new(
            ProviderId::new(provider)?,
            ModelId::new(model)?,
            crate::model::BaseModelId::new(model)?,
            crate::model::ModelFamilyId::new(provider)?,
            None,
        ))
    }

    fn parts() -> Option<CompactionDescriptorParts> {
        Some(CompactionDescriptorParts {
            id: CompactionId::new("cmp-1")?,
            source_session: "ses_1".parse().ok()?,
            source_epoch: ContextEpoch::FIRST,
            target_window: ContextWindowId::new("win-1")?,
            target_epoch: ContextEpoch::FIRST.next()?,
            trigger: CompactionTrigger::ContextOverflow,
            retention_policy: RetentionPolicyVersion::new("ret-1")?,
            summary: CompactionSummary::ClientReadable {
                artifact: artifact("sum-1")?,
            },
            retained: RetainedHistory::References {
                artifacts: vec![artifact("hist-1")?],
            },
            baseline: artifact("base-1")?,
            next_request: NextRequestRef::new(StepNumber::new(1)?, artifact("req-1")?),
        })
    }

    #[test]
    fn a_compaction_that_does_not_advance_the_epoch_is_no_progress(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut stalled = parts().ok_or("parts")?;
        stalled.target_epoch = stalled.source_epoch;
        assert_eq!(
            CompactionDescriptor::new(stalled),
            Err(ContextFailure::CompactionNoProgress),
            "no-progress is a typed outcome, not a committed descriptor"
        );
        Ok(())
    }

    #[test]
    fn a_descriptor_carries_the_reference_that_reassembles_the_next_request(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Law §4.2 lists the Step/request-context reference among the descriptor's
        // coordinates. F4A landed nine and omitted this one. The witness is that a
        // COMMITTED descriptor still exposes it: a compaction that succeeded but
        // could not say which Step resumes, or from which prepared context, has
        // ended one epoch without enabling the next.
        let descriptor = match CompactionDescriptor::new(parts().ok_or("parts")?) {
            Ok(descriptor) => descriptor,
            Err(failure) => return Err(format!("advancing parts must commit: {failure:?}").into()),
        };
        assert_eq!(descriptor.next_request().step().value(), 1);
        assert_eq!(
            descriptor.next_request().context().id().as_str(),
            "req-1",
            "the descriptor must reference the exact prepared request context"
        );
        Ok(())
    }

    #[test]
    fn a_provider_bound_summary_refuses_a_foreign_issuance_and_names_the_recovery(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // §4.3: a provider-bound opaque summary cannot be replayed against an
        // incompatible target, and the provider-neutral text strategy is the
        // recovery path. `is_redeemable_by` answers only yes/no, and a bare
        // `false` at this seam is indistinguishable from "there is no summary".
        // This witnesses that the REFUSAL carries the typed outcome instead.
        //
        // Both refs come from ONE loaded catalog for the same reason the
        // neighbouring redeemability test does: redeemability is same-ISSUANCE.
        let catalog = crate::model::ModelCatalog::load(
            crate::model::ModelCatalogVersion::new("release-1").ok_or("version")?,
            vec![
                crate::model::ModelCatalogRecord::new(
                    ProviderId::new("anthropic").ok_or("provider")?,
                    ModelId::new("claude-opus-5").ok_or("model")?,
                    crate::model::BaseModelId::new("claude-opus-5").ok_or("base")?,
                    crate::model::ModelFamilyId::new("claude").ok_or("family")?,
                    None,
                ),
                crate::model::ModelCatalogRecord::new(
                    ProviderId::new("openai").ok_or("provider")?,
                    ModelId::new("gpt-x").ok_or("model")?,
                    crate::model::BaseModelId::new("gpt-x").ok_or("base")?,
                    crate::model::ModelFamilyId::new("gpt").ok_or("family")?,
                    None,
                ),
            ],
        )
        .ok_or("catalog")?;
        let issued = |provider: &str, model: &str| -> Option<ModelRef> {
            Some(ModelRef::from_catalog(catalog.entry(
                &ProviderId::new(provider)?,
                &ModelId::new(model)?,
                None,
            )?))
        };

        let bound_target = issued("anthropic", "claude-opus-5").ok_or("model")?;
        let foreign = issued("openai", "gpt-x").ok_or("model")?;
        let summary = CompactionSummary::ProviderBound {
            artifact: artifact("sum-bound").ok_or("artifact")?,
            compatibility: SummaryCompatibility::bind(&bound_target),
        };

        let refused = summary.redeem_for(&foreign);
        assert!(refused.needs_neutral_recovery());
        assert_eq!(refused.artifact(), None, "a refusal must not yield content");
        assert_eq!(
            refused.failure(),
            Some(ContextFailure::CompactionIncompatibleSummary),
            "the refusal must carry the typed outcome, not a bare no"
        );

        // It must also name WHICH two sets of coordinates are incompatible. A
        // refusal that knows only that something was wrong cannot tell the
        // owner whether to rebuild neutrally, re-target, or refuse the whole
        // compaction, and the two sides are not interchangeable: bound is
        // where the content came from, requested is where it was about to be
        // replayed. Both are compatibility DATA, so the report reads the same
        // after a restart as before one.
        let incompatibility = refused.incompatibility().ok_or("incompatibility")?;
        assert_eq!(
            incompatibility.bound(),
            &SummaryCompatibility::bind(&bound_target),
            "the refusal must name the coordinates the artifact is bound to"
        );
        assert_eq!(
            incompatibility.requested(),
            &SummaryCompatibility::bind(&foreign),
            "the refusal must name the coordinates redemption was attempted for"
        );

        // The same artifact IS redeemable by its own coordinates, or the
        // refusal above would prove nothing about the coordinate check.
        assert!(summary.redeem_for(&bound_target).artifact().is_some());

        // Provider-neutral text stays first-class: it redeems against the exact
        // foreign issuance that just refused the bound summary.
        let neutral = CompactionSummary::ClientReadable {
            artifact: artifact("sum-neutral").ok_or("artifact")?,
        };
        assert!(neutral.redeem_for(&foreign).artifact().is_some());
        assert!(!neutral.redeem_for(&foreign).needs_neutral_recovery());
        Ok(())
    }

    #[test]
    fn a_compaction_that_moves_the_epoch_backwards_is_also_no_progress(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The refusal is `<=`, not `==`. A descriptor whose target epoch went
        // BACKWARDS would otherwise commit while destroying monotonicity, and
        // an equality-only guard would let it through.
        let mut regressed = parts().ok_or("parts")?;
        regressed.source_epoch = ContextEpoch::FIRST.next().ok_or("second epoch")?;
        regressed.target_epoch = ContextEpoch::FIRST;
        assert_eq!(
            CompactionDescriptor::new(regressed),
            Err(ContextFailure::CompactionNoProgress)
        );
        Ok(())
    }

    #[test]
    fn the_no_progress_refusal_is_carried_by_the_existing_terminal_vocabulary(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The point of the typed outcome is that the reason survives the seam.
        // An `Option` forced the caller to invent a reason here; this proves
        // the refusal reaches a Step terminal without restatement.
        let mut stalled = parts().ok_or("parts")?;
        stalled.target_epoch = stalled.source_epoch;
        let failure = match CompactionDescriptor::new(stalled) {
            Err(failure) => failure,
            Ok(_) => unreachable!("a stalled epoch cannot produce a descriptor"),
        };
        assert_eq!(
            crate::terminality::StepTerminalReason::ContextFailure { failure },
            crate::terminality::StepTerminalReason::ContextFailure {
                failure: ContextFailure::CompactionNoProgress,
            }
        );
        assert_ne!(
            failure,
            ContextFailure::CompactionFailed,
            "no-progress is not the same fact as a failed compaction attempt"
        );
        Ok(())
    }

    #[test]
    fn a_well_formed_descriptor_references_rather_than_inlines(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let descriptor = CompactionDescriptor::new(parts().ok_or("parts")?)
            .map_err(|failure| format!("descriptor refused: {failure:?}"))?;
        assert!(descriptor.target_epoch() > descriptor.source_epoch());
        match descriptor.retained() {
            RetainedHistory::References { artifacts } => {
                assert_eq!(artifacts.len(), 1, "retained history is by reference");
            }
            RetainedHistory::DeterministicSelection { .. } => {
                unreachable!("constructed with references")
            }
        }
        Ok(())
    }

    #[test]
    fn a_provider_bound_summary_refuses_an_incompatible_target(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // F4B1V CORRECTION. This test previously asserted that redeemability
        // was same-ISSUANCE, so a ref for the same endpoint from a different
        // catalog load did NOT redeem. That law was right for an in-memory
        // contract and is wrong for a durable one: issuance identity is an
        // `Arc` snapshot that cannot survive a restart, so an issuance rule
        // would make every provider-bound summary unredeemable after exactly
        // the event compaction exists to survive. The durable rule is
        // COMPATIBILITY — provider, model, variant — and the catalog version
        // is recorded as audit provenance that no rule reads.
        //
        // Refs still come from a real loaded catalog because the BIND route
        // derives from a catalog-vouched target rather than authored strings.
        let catalog = crate::model::ModelCatalog::load(
            crate::model::ModelCatalogVersion::new("release-1").ok_or("version")?,
            vec![
                crate::model::ModelCatalogRecord::new(
                    ProviderId::new("anthropic").ok_or("provider")?,
                    ModelId::new("claude-opus-5").ok_or("model")?,
                    crate::model::BaseModelId::new("claude-opus-5").ok_or("base")?,
                    crate::model::ModelFamilyId::new("claude").ok_or("family")?,
                    None,
                ),
                crate::model::ModelCatalogRecord::new(
                    ProviderId::new("openai").ok_or("provider")?,
                    ModelId::new("gpt-x").ok_or("model")?,
                    crate::model::BaseModelId::new("gpt-x").ok_or("base")?,
                    crate::model::ModelFamilyId::new("gpt").ok_or("family")?,
                    None,
                ),
            ],
        )
        .ok_or("catalog")?;
        let issued = |provider: &str, model: &str| -> Option<ModelRef> {
            Some(ModelRef::from_catalog(catalog.entry(
                &ProviderId::new(provider)?,
                &ModelId::new(model)?,
                None,
            )?))
        };

        let bound = CompactionSummary::ProviderBound {
            artifact: artifact("sum-1").ok_or("artifact")?,
            compatibility: SummaryCompatibility::bind(
                &issued("anthropic", "claude-opus-5").ok_or("model")?,
            ),
        };
        assert!(
            bound.is_redeemable_by(&issued("anthropic", "claude-opus-5").ok_or("model")?),
            "the same coordinates redeem it"
        );
        assert!(
            !bound.is_redeemable_by(&issued("openai", "gpt-x").ok_or("model")?),
            "an opaque provider summary is not portable across provider/model"
        );
        // THE CORRECTED LAW, and the one a restart depends on: the same
        // endpoint from a DIFFERENT catalog issuance — a different `Arc`
        // snapshot, and here a different recorded version string too — MUST
        // redeem. `ModelRef::new` is the test-only constructor that mints a
        // fresh snapshot per call, so this ref is provably not the issuance
        // the summary was bound under.
        assert!(
            bound.is_redeemable_by(&model("anthropic", "claude-opus-5").ok_or("model")?),
            "the same endpoint from another catalog issuance must still redeem: \
             an issuance rule would not survive a restart"
        );
        Ok(())
    }

    #[test]
    fn each_redemption_coordinate_is_load_bearing() -> Result<(), Box<dyn std::error::Error>> {
        // The redemption rule is a COMPOSITE key over three coordinates, and a
        // single "a foreign target refuses" witness proves only that the
        // conjunction is not vacuous. It cannot tell which coordinate did the
        // refusing: the neighbouring test's foreign target differs in provider
        // AND model at once, so dropping either one from the rule leaves it
        // green. Variant had no separating witness at all.
        //
        // Each target below differs from the bound coordinates in EXACTLY one
        // dimension, so each dimension has its own named assertion and its own
        // mutant.
        let catalog = crate::model::ModelCatalog::load(
            crate::model::ModelCatalogVersion::new("release-1").ok_or("version")?,
            vec![
                crate::model::ModelCatalogRecord::new(
                    ProviderId::new("anthropic").ok_or("provider")?,
                    ModelId::new("claude-opus-5").ok_or("model")?,
                    crate::model::BaseModelId::new("claude-opus-5").ok_or("base")?,
                    crate::model::ModelFamilyId::new("claude").ok_or("family")?,
                    None,
                ),
                // Same provider and model; variant is the only difference.
                crate::model::ModelCatalogRecord::new(
                    ProviderId::new("anthropic").ok_or("provider")?,
                    ModelId::new("claude-opus-5").ok_or("model")?,
                    crate::model::BaseModelId::new("claude-opus-5").ok_or("base")?,
                    crate::model::ModelFamilyId::new("claude").ok_or("family")?,
                    Some(crate::model::ModelVariantId::new("thinking").ok_or("variant")?),
                ),
                // Same model; provider is the only difference.
                crate::model::ModelCatalogRecord::new(
                    ProviderId::new("bedrock").ok_or("provider")?,
                    ModelId::new("claude-opus-5").ok_or("model")?,
                    crate::model::BaseModelId::new("claude-opus-5").ok_or("base")?,
                    crate::model::ModelFamilyId::new("claude").ok_or("family")?,
                    None,
                ),
                // Same provider; model is the only difference.
                crate::model::ModelCatalogRecord::new(
                    ProviderId::new("anthropic").ok_or("provider")?,
                    ModelId::new("claude-sonnet-5").ok_or("model")?,
                    crate::model::BaseModelId::new("claude-sonnet-5").ok_or("base")?,
                    crate::model::ModelFamilyId::new("claude").ok_or("family")?,
                    None,
                ),
            ],
        )
        .ok_or("catalog")?;
        let issued = |provider: &str, model: &str, variant: Option<&str>| -> Option<ModelRef> {
            let variant = match variant {
                Some(value) => Some(crate::model::ModelVariantId::new(value)?),
                None => None,
            };
            Some(ModelRef::from_catalog(catalog.entry(
                &ProviderId::new(provider)?,
                &ModelId::new(model)?,
                variant.as_ref(),
            )?))
        };

        let bound_target = issued("anthropic", "claude-opus-5", None).ok_or("model")?;
        let summary = CompactionSummary::ProviderBound {
            artifact: artifact("sum-coordinates").ok_or("artifact")?,
            compatibility: SummaryCompatibility::bind(&bound_target),
        };

        assert!(
            summary.is_redeemable_by(&bound_target),
            "the exact bound coordinates must redeem, or every refusal below \
             would be a blanket no rather than a coordinate decision"
        );
        assert!(
            !summary.is_redeemable_by(&issued("bedrock", "claude-opus-5", None).ok_or("model")?),
            "a different PROVIDER must not redeem: the same model name behind \
             another provider is another provider's opaque content"
        );
        assert!(
            !summary
                .is_redeemable_by(&issued("anthropic", "claude-sonnet-5", None).ok_or("model")?),
            "a different MODEL must not redeem"
        );
        assert!(
            !summary.is_redeemable_by(
                &issued("anthropic", "claude-opus-5", Some("thinking")).ok_or("model")?
            ),
            "a different VARIANT must not redeem: a bound variant is part of \
             the compatibility identity, not decoration"
        );
        Ok(())
    }

    #[test]
    fn the_recorded_catalog_version_is_audit_provenance_and_gates_nothing(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The audit version is REQUIRED in the durable value and must be
        // provably inert in redemption. Without this witness, a rule that
        // silently compared versions would look identical from outside until
        // the first catalog reload refused a legitimate summary.
        let bound_ref = model("anthropic", "claude-opus-5").ok_or("model")?;
        let other_issuance = model("anthropic", "claude-opus-5").ok_or("model")?;
        let compatibility = SummaryCompatibility::bind(&bound_ref);

        // Same coordinates, and the two refs are separate issuances.
        assert!(!bound_ref.is_same_issuance(&other_issuance));
        assert!(compatibility.matches_target(&other_issuance));

        // The version IS carried — it is audit provenance, not an omission.
        assert_eq!(
            compatibility.catalog_version(),
            bound_ref.catalog_version(),
            "the binding records which catalog vouched for the coordinates"
        );

        // And a value reconstituted from persisted parts with a DIFFERENT
        // recorded version still redeems on coordinates alone. This is the
        // shape the restart adversary takes: stored bytes, a new catalog, no
        // issuance anywhere.
        let reconstituted = SummaryCompatibility::from_validated_parts(
            compatibility.provider().clone(),
            compatibility.model().clone(),
            compatibility.variant().cloned(),
            crate::model::ModelCatalogVersion::new("release-99").ok_or("version")?,
        );
        assert_ne!(
            reconstituted.catalog_version(),
            compatibility.catalog_version()
        );
        assert!(
            reconstituted.matches_target(&other_issuance),
            "a differing audit version must not change redemption"
        );
        Ok(())
    }

    #[test]
    fn a_client_readable_summary_is_the_portable_recovery_path(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let text = CompactionSummary::ClientReadable {
            artifact: artifact("sum-1").ok_or("artifact")?,
        };
        assert!(text.is_redeemable_by(&model("openai", "gpt-x").ok_or("model")?));
        assert!(text.is_redeemable_by(&model("anthropic", "claude-opus-5").ok_or("model")?));
        Ok(())
    }

    #[test]
    fn absent_and_unavailable_are_distinct_contribution_states(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let absent = ContributionState::Absent;
        let unavailable = ContributionState::Unavailable {
            reason: ContributionUnavailable::SourceUnreadable,
        };
        assert_ne!(absent, unavailable);
        assert!(absent.artifact().is_none());
        assert!(unavailable.artifact().is_none());

        let available = ContributionState::Available {
            artifact: artifact("c-1").ok_or("artifact")?,
        };
        assert!(available.artifact().is_some());
        Ok(())
    }

    #[test]
    fn trust_is_a_label_not_a_claim() {
        assert_ne!(TrustLabel::Untrusted, TrustLabel::Trusted);
        assert_ne!(TrustLabel::ProjectContent, TrustLabel::Trusted);
    }

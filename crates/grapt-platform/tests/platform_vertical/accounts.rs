//! Credential source refusals and multi-account composition.

// Fixtures shared by the platform vertical batteries. Included rather than
// imported: integration tests cannot reach into the crate, so a shared helper
// has nowhere central to live.

use std::path::Path;

use grapt_platform::{
    ConfigPublicationError, CredentialRefusal, CredentialResolutionRequest, PlatformOwner,
    PlatformRefusal, PreparedProviderInputs, ProviderInputParts, PublishedConfig,
};
use grapt_provider::ProtocolFamily;

use super::shared::*;

#[test]
fn local_plaintext_source_refuses_absent_directory_empty_unsafe_and_links() -> TestResult {
    let root = scratch("credential-source-refusals")?;
    ready_database(&root)?;
    write_config(&root, "open-ready", "build-a", &[], None)?;
    let source = root.join("provider.credential");

    std::fs::remove_file(&source)?;
    assert!(
        matches!(
            platform_refusal(&root)?,
            PlatformRefusal::Credential(CredentialRefusal::Absent)
        ),
        "absent credential source did not refuse as absent"
    );

    std::fs::create_dir(&source)?;
    assert!(
        matches!(
            platform_refusal(&root)?,
            PlatformRefusal::Credential(CredentialRefusal::NotAFile)
        ),
        "credential directory did not refuse as a non-file"
    );
    std::fs::remove_dir(&source)?;

    std::fs::write(&source, b"")?;
    assert!(
        matches!(
            platform_refusal(&root)?,
            PlatformRefusal::Credential(CredentialRefusal::Empty)
        ),
        "empty credential source did not refuse as empty"
    );

    std::fs::write(&source, b"sentinel-provider-secret\nforged: header")?;
    let refusal = platform_refusal(&root)?;
    assert!(!format!("{refusal:?} {refusal}").contains("sentinel-provider-secret"));
    assert!(matches!(
        &refusal,
        PlatformRefusal::Credential(CredentialRefusal::Capability(
            grapt_provider::CredentialError::SecretNotHeaderSafe
        ))
    ));

    std::fs::remove_file(&source)?;
    #[cfg(unix)]
    {
        let outside = root.join("outside.credential");
        std::fs::write(&outside, b"sentinel-provider-secret")?;
        symlink_file(&outside, &source)?;
    }
    #[cfg(windows)]
    {
        let outside = root.join("outside-credential-directory");
        std::fs::create_dir(&outside)?;
        symlink_dir(&outside, &source)?;
    }
    assert!(
        matches!(
            platform_refusal(&root)?,
            PlatformRefusal::Credential(CredentialRefusal::SourceLinked)
        ),
        "linked credential source did not refuse as linked"
    );

    let linked_root = scratch("credential-linked-root")?;
    std::fs::remove_dir(&linked_root)?;
    symlink_dir(&root, &linked_root)?;
    assert!(
        matches!(
            platform_refusal(&linked_root)?,
            PlatformRefusal::Credential(CredentialRefusal::RootLinked)
        ),
        "linked credential root did not refuse as linked"
    );

    remove_source_link(&source)?;
    remove_link_dir(&linked_root)?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn local_plaintext_source_refuses_an_unreadable_locked_file() -> TestResult {
    use std::os::windows::fs::OpenOptionsExt;

    let root = scratch("credential-source-locked")?;
    ready_database(&root)?;
    write_config(&root, "open-ready", "build-a", &[], None)?;
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(root.join("provider.credential"))?;
    assert!(matches!(
        platform_refusal(&root)?,
        PlatformRefusal::Credential(CredentialRefusal::Read(_))
    ));
    drop(locked);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// J01 — configured multi-provider catalog and per-account credential authority.
//
// The witnesses below compose a SECOND configured connection and prove the
// three things the acceptance clause actually rests on: a second account
// contributes its own issuable rows, selection resolves through the account
// that owns the selected provider, and an unconfigured provider is refused by
// name rather than quietly served by the default connection.

/// A three-provider public catalog. Breadth here is metadata only:
/// `unconfigured` is described exactly like the others and still has no
/// configured account.
fn multi_account_catalog() -> &'static str {
    r#"{
  "openai":{"id":"openai","name":"OpenAI","npm":"@ai-sdk/openai","models":{
    "gpt-5.6":{"id":"gpt-5.6","name":"GPT-5.6","family":"gpt-5"}
  }},
  "openrouter":{"id":"openrouter","name":"OpenRouter","npm":"@openrouter/ai-sdk-provider","models":{
    "vendor-model-a":{"id":"vendor-model-a","name":"Model A","family":"model-a"}
  }},
  "unconfigured":{"id":"unconfigured","name":"Unconfigured","npm":"x","models":{
    "never":{"id":"never","name":"Never","family":"never"}
  }}
}"#
}

fn write_multi_account_config(root: &Path) -> TestResult {
    std::fs::write(
        root.join("grapt.jsonc"),
        r#"{
  "store":{"authority":"open-ready"},
  "product":{"build":"build-a"},
  "catalog":{"source":"models.dev-chatgpt"},
  "provider":{
    "id":"openai","model":"gpt-5.6","base_model":"gpt-5.6","family":"gpt-5",
    "route":"responses","account":"account-a",
    "credential_version":"credential-v1","policy_version":"df0-policy-v1",
    "accounts":[
      {"id":"openrouter","route":"route-openrouter","account":"account-b",
       "credential_version":"credential-b1","policy_version":"df0-policy-v1",
       "protocol_family":"openai-compatible",
       "endpoint":"https://openrouter.ai/api/v1",
       "credential":"openrouter.credential",
       "discovery":"openrouter-account-models.json"}
    ]
  }
}"#,
    )?;
    std::fs::write(
        root.join("provider.credential"),
        "sentinel-provider-secret\n",
    )?;
    std::fs::write(
        root.join("openrouter.credential"),
        "sentinel-openrouter-secret\n",
    )?;
    std::fs::write(root.join("model-catalog.json"), multi_account_catalog())?;
    std::fs::write(
        root.join("account-models.json"),
        r#"{"models":[{"slug":"gpt-5.6","visibility":"list","context_window":272000,
            "supported_reasoning_levels":[]}]}"#,
    )?;
    std::fs::write(
        root.join("openrouter-account-models.json"),
        r#"{"models":[{"slug":"vendor-model-a","visibility":"list","context_window":128000,
            "supported_reasoning_levels":[]}]}"#,
    )?;
    Ok(())
}

#[test]
fn store_free_bootstrap_binds_accounts_without_a_database_and_refuses_renewal() -> TestResult {
    let root = scratch("store-free-provider-inputs")?;
    write_multi_account_config(&root)?;
    assert!(matches!(
        PreparedProviderInputs::prepare_for_build(&root, Some("another-build")),
        Err(PlatformRefusal::HandoverBuildChanged)
    ));
    for obstruct_database in [false, true] {
        if obstruct_database {
            // A database cannot be opened at this path; provider preparation
            // must work independently of its existence or readiness.
            std::fs::create_dir(root.join("database.sqlite3"))?;
        }
        let ProviderInputParts {
            targets,
            build,
            credential: primary,
            secondary_credentials: secondary,
        } = PreparedProviderInputs::prepare_for_build(&root, Some("build-a"))?.into_components()?;
        assert_eq!(build.as_str(), "build-a");
        assert_eq!(secondary.len(), 1);
        for (provider, credential) in
            std::iter::once((provider_id("openai"), primary)).chain(secondary)
        {
            let target = if provider.as_str() == "openai" {
                targets.issue()?
            } else {
                targets.issue_selected(&provider, &model_id("vendor-model-a"), None)?
            };
            let request = CredentialResolutionRequest::new(
                &target,
                credential.record().clone(),
                credential.protocol_family(),
                credential.placement(),
            );
            if obstruct_database {
                assert!(matches!(
                    credential.into_refreshable(request),
                    Err(CredentialRefusal::RenewalDisabled)
                ));
            } else {
                let resolved = credential.resolve(request)?;
                assert_eq!(resolved.account(), target.account());
            }
        }
        if !obstruct_database {
            assert!(!root.join("database.sqlite3").exists());
        }
        assert!(!root.join("server.log").exists());
        assert!(!root.join(grapt_login::chatgpt::STATE_FILE).exists());
        assert!(!root.join("chatgpt.lock").exists());
    }
    assert_eq!(
        std::fs::read_to_string(root.join("provider.credential"))?,
        "sentinel-provider-secret\n"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_second_configured_account_is_connected_and_issues_through_its_own_coordinates() -> TestResult {
    let root = scratch("multi-account-positive")?;
    ready_database(&root)?;
    write_multi_account_config(&root)?;

    let published = PublishedConfig::load(&root)?;
    // Each connection keeps its OWN typed account and credential version; the
    // second account did not inherit the primary's authority.
    let primary = published.credentials();
    assert_eq!(primary.provider().as_str(), "openai");
    assert_eq!(primary.account().as_str(), "account-a");
    assert_eq!(primary.protocol_family(), ProtocolFamily::ChatGptCodex);
    let secondary = published
        .credentials_for(&provider_id("openrouter"))
        .ok_or("the configured openrouter account has no grant")?;
    assert_eq!(
        secondary.account().as_str(),
        "account-b",
        "a second account must keep its own typed account, not inherit the primary's"
    );
    assert_eq!(secondary.version().as_str(), "credential-b1");
    assert_eq!(secondary.route().as_str(), "route-openrouter");
    assert_eq!(
        secondary.protocol_family(),
        ProtocolFamily::OpenAiCompatible
    );
    // Reading a provider's metadata never connects it.
    assert!(published
        .credentials_for(&provider_id("unconfigured"))
        .is_none());
    drop(published);

    let owner =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    let parts = owner.into_provider_parts()?;
    let (store, targets, _build, _credential, _first_contact) = parts.into_components();
    drop(store);

    let inventory = targets
        .inventory()
        .ok_or("a composed inventory is required")?;
    assert_eq!(
        inventory
            .connected()
            .iter()
            .map(|provider| provider.as_str())
            .collect::<Vec<_>>(),
        vec!["openai", "openrouter"],
        "the connected set is the configured accounts, not the catalog"
    );
    assert_eq!(
        inventory.providers().len(),
        3,
        "public metadata breadth is retained even though only two are connected"
    );
    let routed: Vec<_> = inventory
        .available()
        .iter()
        .map(|model| (model.provider().as_str(), model.id().as_str()))
        .collect();
    assert_eq!(
        routed,
        vec![("openai", "gpt-5.6"), ("openrouter", "vendor-model-a")],
        "each issuable row must carry the provider it is reachable through"
    );

    // The default connection still issues exactly as it did single-account.
    let default_target = targets.issue()?;
    assert_eq!(default_target.provider().as_str(), "openai");
    assert_eq!(default_target.account().as_str(), "account-a");

    // Selecting the second provider resolves through ITS account, route and
    // credential version — not the default connection's.
    let selected = targets.issue_selected(
        &provider_id("openrouter"),
        &model_id("vendor-model-a"),
        None,
    )?;
    assert_eq!(selected.provider().as_str(), "openrouter");
    assert_eq!(
        selected.account().as_str(),
        "account-b",
        "selection must resolve through the account that owns the selected provider"
    );
    assert_eq!(selected.route().as_str(), "route-openrouter");
    assert_eq!(selected.credential_version().as_str(), "credential-b1");
    assert_eq!(
        targets.protocol_family_for(&provider_id("openrouter")),
        Some(ProtocolFamily::OpenAiCompatible),
        "a second connection must be spoken to in its own protocol, not the default account's"
    );
    assert_eq!(
        targets.endpoint_for(&provider_id("openrouter")),
        Some("https://openrouter.ai/api/v1"),
        "an openai-compatible connection must carry the endpoint it is reached at"
    );
    // The primary keeps its family default rather than borrowing that base.
    assert_eq!(targets.endpoint_for(&provider_id("openai")), None);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn an_unconfigured_provider_is_refused_by_name_and_never_served_by_the_default_account(
) -> TestResult {
    let root = scratch("multi-account-unconfigured")?;
    ready_database(&root)?;
    write_multi_account_config(&root)?;
    let owner =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    let parts = owner.into_provider_parts()?;
    let (store, targets, _build, _credential, _first_contact) = parts.into_components();
    drop(store);

    // "unconfigured" is in the public catalog with a model, and has no account.
    let refusal = targets.issue_selected(&provider_id("unconfigured"), &model_id("never"), None);
    assert!(
        matches!(
            refusal,
            Err(ConfigPublicationError::UnconfiguredProvider(ref named))
                if named == "unconfigured"
        ),
        "an unconfigured provider must be refused by name, not silently rerouted"
    );
    assert!(targets
        .protocol_family_for(&provider_id("unconfigured"))
        .is_none());
    // A model that belongs to another connection is not reachable by naming a
    // configured provider either: the whole coordinate must exist.
    assert!(targets
        .issue_selected(&provider_id("openai"), &model_id("vendor-model-a"), None)
        .is_err());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// Codex sends the account inventory's default reasoning level whenever the
/// user chose none (Luna: "medium"); Grapt sent no effort at all
///. A selection without a variant now issues that
/// default, an explicit one still wins, and a default the model does not
/// list is ignored.
#[test]
fn an_unselected_effort_issues_the_account_default_reasoning_level() -> TestResult {
    let root = scratch("multi-account-default-effort")?;
    ready_database(&root)?;
    write_multi_account_config(&root)?;
    let levels = r#"[{"effort":"low"},{"effort":"medium"},{"effort":"high"}]"#;
    std::fs::write(
        root.join("account-models.json"),
        format!(
            r#"{{"models":[{{"slug":"gpt-5.6","visibility":"list","context_window":272000,
                "default_reasoning_level":"medium","supported_reasoning_levels":{levels}}}]}}"#
        ),
    )?;
    std::fs::write(
        root.join("openrouter-account-models.json"),
        format!(
            r#"{{"models":[{{"slug":"vendor-model-a","visibility":"list","context_window":128000,
                "default_reasoning_level":"turbo","supported_reasoning_levels":{levels}}}]}}"#
        ),
    )?;
    let owner =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    let parts = owner.into_provider_parts()?;
    let (store, targets, _build, _credential, _first_contact) = parts.into_components();
    drop(store);

    let variant = |target: &grapt_types::ResolvedModelTarget| {
        target
            .model_ref()
            .variant()
            .map(|variant| variant.as_str().to_owned())
    };
    assert_eq!(variant(&targets.issue()?).as_deref(), Some("medium"));
    let openai = provider_id("openai");
    let model = model_id("gpt-5.6");
    assert_eq!(
        variant(&targets.issue_selected(&openai, &model, None)?).as_deref(),
        Some("medium")
    );
    let high = grapt_types::ModelVariantId::new("high").ok_or("variant")?;
    assert_eq!(
        variant(&targets.issue_selected(&openai, &model, Some(&high))?).as_deref(),
        Some("high"),
        "an explicit selection must win over the default"
    );
    assert_eq!(
        variant(&targets.issue_selected(
            &provider_id("openrouter"),
            &model_id("vendor-model-a"),
            None
        )?),
        None,
        "a default the model does not list must not be invented"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn account_composition_refuses_contradictory_or_unbounded_declarations() -> TestResult {
    let root = scratch("multi-account-refusals")?;
    ready_database(&root)?;
    write_multi_account_config(&root)?;
    let base = std::fs::read_to_string(root.join("grapt.jsonc"))?;
    // Each mutation is a distinct way a second account could borrow authority
    // it was not granted, or escape a bound, and each names the EXACT refusal
    // it must produce.
    //
    // Asserting only `is_err()` was not enough, and a surviving mutant proved
    // it: several of these invariants are defended twice, once where the
    // configuration is composed and again where the inventory is decoded. A
    // test that accepts any error cannot tell which layer answered, so
    // disabling the composition check left the witness green while the guard
    // it names was gone. The expected variant is what makes each row load-bearing.
    for (label, mutated, expected) in [
        (
            "a second account claiming the primary provider",
            base.replace("\"id\":\"openrouter\"", "\"id\":\"openai\""),
            // Cast once so the array is a slice of fn pointers; every later
            // closure coerces to it.
            (|error: &ConfigPublicationError| {
                matches!(error, ConfigPublicationError::DuplicateAccount { .. })
            }) as fn(&ConfigPublicationError) -> bool,
        ),
        (
            "a second account reusing the primary credential input",
            base.replace("openrouter.credential", "provider.credential"),
            |error: &ConfigPublicationError| {
                matches!(error, ConfigPublicationError::DuplicateAccount { .. })
            },
        ),
        (
            "a second account reusing the primary discovery input",
            base.replace("openrouter-account-models.json", "account-models.json"),
            |error: &ConfigPublicationError| {
                matches!(error, ConfigPublicationError::DuplicateAccount { .. })
            },
        ),
        (
            "a credential input escaping the service root",
            base.replace("openrouter.credential", "../openrouter.credential"),
            |error: &ConfigPublicationError| {
                matches!(
                    error,
                    ConfigPublicationError::Empty {
                        path: "$.provider.accounts[].credential",
                    }
                )
            },
        ),
        (
            "a discovery input escaping the service root",
            base.replace(
                "openrouter-account-models.json",
                "../openrouter-account-models.json",
            ),
            |error: &ConfigPublicationError| matches!(error, ConfigPublicationError::Load(_)),
        ),
        (
            "an unrecognized protocol family",
            base.replace("openai-compatible", "telepathy"),
            |error: &ConfigPublicationError| {
                matches!(error, ConfigPublicationError::ProtocolFamily(_))
            },
        ),
        (
            "an openai-compatible account with no endpoint to reach",
            base.replace(
                "\"endpoint\":\"https://openrouter.ai/api/v1\",
       ",
                "",
            ),
            |error: &ConfigPublicationError| {
                matches!(error, ConfigPublicationError::EndpointRequired(_))
            },
        ),
        (
            "an account whose endpoint is present but empty",
            base.replace("https://openrouter.ai/api/v1", ""),
            |error: &ConfigPublicationError| {
                matches!(
                    error,
                    ConfigPublicationError::Empty {
                        path: "$.provider.accounts[].endpoint",
                    }
                )
            },
        ),
    ] {
        std::fs::write(root.join("grapt.jsonc"), &mutated)?;
        match PublishedConfig::load(&root) {
            Ok(_) => panic!("{label} was admitted"),
            Err(error) => assert!(
                expected(&error),
                "{label} was refused by the wrong guard: {error}"
            ),
        }
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn bootstrap_refusals_name_the_fix_and_the_location() -> TestResult {
    // J02 witness: a user fixing the
    // configuration from the refusal alone must be able to. Each row asserts
    // the exact text, so restoring an old wording reds this test.
    let root = scratch("refusal-wording-witness")?;
    ready_database(&root)?;
    write_multi_account_config(&root)?;
    let base = std::fs::read_to_string(root.join("grapt.jsonc"))?;
    for (mutated, expected) in [
        (
            base.replace("\"id\":\"openrouter\"", "\"id\":\"openai\""),
            "configuration declares provider \"openai\" or one of its inputs twice: \
             accounts[0] and the primary share the provider id; each account must \
             differ from the primary and every other account in provider id, \
             credential source and discovery file",
        ),
        (
            base.replace("openrouter.credential", "provider.credential"),
            "configuration declares provider \"openrouter\" or one of its inputs \
             twice: accounts[0] and the primary share the credential source; each \
             account must differ from the primary and every other account in \
             provider id, credential source and discovery file",
        ),
        (
            base.replace("openrouter-account-models.json", "account-models.json"),
            "configuration declares provider \"openrouter\" or one of its inputs \
             twice: accounts[0] and the primary share the discovery file; each \
             account must differ from the primary and every other account in \
             provider id, credential source and discovery file",
        ),
        (
            base.replace(
                "\"id\":\"openai\",\"model\"",
                "\"id\":\"qwen-local\",\"model\"",
            ),
            "catalog source \"models.dev-chatgpt\" is not composed with provider id \
             \"qwen-local\" and protocol family chatgpt-codex; composed pairs: provider id \"openai\" with catalog source \"models.dev-chatgpt\" (protocol family chatgpt-codex), provider id \"anthropic\" with catalog source \"models.dev-anthropic\" (protocol family anthropic-messages), provider id \"openai\" with catalog source \"models.dev-openai\" (protocol family openai-responses); use one of those, or remove catalog.source and declare your models in provider.variants",
        ),
        (
            base.replace(
                "\"source\":\"models.dev-chatgpt\"",
                "\"source\":\"models.dev\"",
            ),
            "catalog source \"models.dev\" is not composed with provider id \
             \"openai\" and protocol family chatgpt-codex; composed pairs: provider id \"openai\" with catalog source \"models.dev-chatgpt\" (protocol family chatgpt-codex), provider id \"anthropic\" with catalog source \"models.dev-anthropic\" (protocol family anthropic-messages), provider id \"openai\" with catalog source \"models.dev-openai\" (protocol family openai-responses); use one of those, or remove catalog.source and declare your models in provider.variants",
        ),
    ] {
        std::fs::write(root.join("grapt.jsonc"), &mutated)?;
        match PublishedConfig::load(&root) {
            Ok(_) => panic!("a refused configuration was admitted"),
            Err(error) => assert_eq!(
                error.to_string(),
                expected,
                "the refusal must state what to change and where"
            ),
        }
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

fn provider_id(value: &str) -> grapt_types::ProviderId {
    match grapt_types::ProviderId::new(value) {
        Some(id) => id,
        None => panic!("fixture provider id {value:?} is invalid"),
    }
}

fn model_id(value: &str) -> grapt_types::ModelId {
    match grapt_types::ModelId::new(value) {
        Some(id) => id,
        None => panic!("fixture model id {value:?} is invalid"),
    }
}

/// The real two-account composition, against the campaign's provisioned inputs.
///
/// Evidence-bearing and `#[ignore]`d in the established style: it pins the
/// absolute provisioned service root and PANICS loudly when that root is
/// absent, rather than skipping into a pass. It reads a real live-discovery
/// snapshot produced by scripts/daily-use/provision_openrouter.py; it does not
/// contact any provider itself and reads no credential material.
///
/// Run on the provisioning machine with:
///   cargo test -p grapt-platform --test platform_vertical -- --include-ignored
#[test]
#[ignore = "requires the campaign's provisioned two-account service root"]
fn the_provisioned_two_account_composition_connects_both_real_providers() -> TestResult {
    let configured = std::env::var_os("GRAPT_TWO_ACCOUNT_SERVICE_ROOT")
        .ok_or("set GRAPT_TWO_ACCOUNT_SERVICE_ROOT to the provisioned service root")?;
    let root = Path::new(&configured);
    if !root.join("grapt.jsonc").is_file() {
        panic!(
            "the provisioned two-account service root is absent at {}; \
             run scripts/daily-use/provision_openrouter.py before this witness",
            root.display()
        );
    }
    let published = PublishedConfig::load(root)?;

    // Both accounts hold their own coordinates. Nothing is inherited.
    let primary = published.credentials();
    assert_eq!(primary.provider().as_str(), "openai");
    assert_eq!(primary.protocol_family(), ProtocolFamily::ChatGptCodex);
    let secondary = published
        .credentials_for(&provider_id("openrouter"))
        .ok_or("the provisioned openrouter account has no grant")?;
    assert_eq!(secondary.account().as_str(), "account-openrouter");
    assert_eq!(
        secondary.protocol_family(),
        ProtocolFamily::OpenAiCompatible,
        "openrouter must be spoken to as an openai-compatible endpoint"
    );

    let inventory = published
        .inventory()
        .ok_or("the provisioned catalog source must compose an inventory")?;
    assert_eq!(
        inventory
            .connected()
            .iter()
            .map(|provider| provider.as_str())
            .collect::<Vec<_>>(),
        vec!["openai", "openrouter"],
        "both provisioned accounts must be connected"
    );

    let mut per_provider: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for model in inventory.available() {
        *per_provider.entry(model.provider().as_str()).or_default() += 1;
    }
    // Real counts from the real snapshots. The ChatGPT account advertises nine
    // models of which seven are visible; the two marked hidden are excluded by
    // discovery, which is the behavior worth pinning here — a hidden model must
    // not become issuable just because a second account joined the composition.
    assert_eq!(
        per_provider.get("openai").copied(),
        Some(7),
        "the provisioned ChatGPT account's visible discovery changed"
    );
    let routed = per_provider.get("openrouter").copied().unwrap_or_default();
    assert!(
        routed > 100,
        "the provisioned OpenRouter account contributed only {routed} issuable models"
    );
    // Metadata breadth still exceeds the connected set, in both directions:
    // the catalog describes providers that have no account at all, and it
    // describes more openrouter models than this account can issue. Reading
    // either fact connects nothing.
    assert!(
        inventory.providers().len() > inventory.connected().len(),
        "the public catalog must describe providers beyond the connected two"
    );
    let described = inventory
        .providers()
        .get("openrouter")
        .and_then(|row| row.get("models"))
        .and_then(serde_json::Value::as_object)
        .map(serde_json::Map::len)
        .unwrap_or_default();
    assert!(
        described >= routed,
        "the catalog describes {described} openrouter models but {routed} are issuable"
    );
    Ok(())
}

/// Both connections pointed at loopback, and the PUBLISHED primary endpoint read
/// back rather than the configuration merely being accepted.
fn write_local_primary_config(root: &Path, primary_endpoint: &str) -> TestResult {
    write_multi_account_config(root)?;
    let base = std::fs::read_to_string(root.join("grapt.jsonc"))?;
    let configured = base.replace(
        "\"route\":\"responses\",\"account\":\"account-a\",",
        &format!(
            "\"route\":\"responses\",\"account\":\"account-a\",\"endpoint\":\"{primary_endpoint}\","
        ),
    );
    assert_ne!(configured, base, "the primary endpoint was not inserted");
    std::fs::write(root.join("grapt.jsonc"), configured)?;
    Ok(())
}

#[test]
fn a_configured_primary_endpoint_is_published_and_an_omitted_one_keeps_the_family_default(
) -> TestResult {
    // Until the primary could carry an endpoint, NO configuration could point it
    // at a local fixture: it was built with `endpoint: None` and the family
    // default is a real provider address. A qualification turn whose model
    // selection failed to bind therefore fell back to the primary and left the
    // host, and nothing in the configuration surface could have stopped it.
    let root = scratch("primary-endpoint-configured")?;
    ready_database(&root)?;
    write_local_primary_config(&root, "https://127.0.0.1:65027")?;

    PublishedConfig::load(&root)?;
    let owner =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    let parts = owner.into_provider_parts()?;
    let (store, targets, _build, _credential, _first_contact) = parts.into_components();
    drop(store);
    assert_eq!(
        targets.endpoint_for(&provider_id("openai")),
        Some("https://127.0.0.1:65027"),
        "the configured primary endpoint must be PUBLISHED, not merely accepted; \
         reading it back is what distinguishes applied from ignored"
    );
    // The secondary is unaffected and still carries its own.
    assert_eq!(
        targets.endpoint_for(&provider_id("openrouter")),
        Some("https://openrouter.ai/api/v1"),
        "configuring the primary must not disturb a secondary's own endpoint"
    );
    std::fs::remove_dir_all(root)?;

    // Omitted is exactly what it always was. This is the compatibility half, and
    // without it the change could have made every primary carry something.
    let root = scratch("primary-endpoint-omitted")?;
    ready_database(&root)?;
    write_multi_account_config(&root)?;
    PublishedConfig::load(&root)?;
    let owner =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    let parts = owner.into_provider_parts()?;
    let (store, targets, _build, _credential, _first_contact) = parts.into_components();
    drop(store);
    assert_eq!(
        targets.endpoint_for(&provider_id("openai")),
        None,
        "an omitted primary endpoint must keep the family default it always used"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn an_invalid_primary_endpoint_is_an_explicit_refusal_and_never_a_silent_default() -> TestResult {
    let root = scratch("primary-endpoint-invalid")?;
    ready_database(&root)?;
    write_local_primary_config(&root, "https://127.0.0.1:65027")?;
    let base = std::fs::read_to_string(root.join("grapt.jsonc"))?;

    for (label, mutated, expected) in [
        (
            "a primary endpoint present but empty",
            base.replace(
                "\"endpoint\":\"https://127.0.0.1:65027\"",
                "\"endpoint\":\"\"",
            ),
            (|error: &ConfigPublicationError| {
                matches!(
                    error,
                    ConfigPublicationError::Empty {
                        path: "$.provider.endpoint",
                    }
                )
            }) as fn(&ConfigPublicationError) -> bool,
        ),
        (
            "a primary endpoint of the wrong type",
            base.replace("\"endpoint\":\"https://127.0.0.1:65027\"", "\"endpoint\":7"),
            |error: &ConfigPublicationError| {
                matches!(
                    error,
                    ConfigPublicationError::WrongType {
                        path: "$.provider.endpoint",
                        ..
                    }
                )
            },
        ),
    ] {
        std::fs::write(root.join("grapt.jsonc"), &mutated)?;
        match PublishedConfig::load(&root) {
            // Falling back to the family default here would be the dangerous
            // outcome: a configuration that MEANT to be local, silently isn't.
            Ok(_) => panic!("{label} was admitted instead of refused"),
            Err(error) => assert!(
                expected(&error),
                "{label} was refused by the wrong guard: {error}"
            ),
        }
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

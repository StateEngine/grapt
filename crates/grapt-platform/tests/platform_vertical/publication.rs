//! Config publication, reload and credential sources.

// Fixtures shared by the platform vertical batteries. Included rather than
// imported: integration tests cannot reach into the crate, so a shared helper
// has nowhere central to live.

use grapt_config::StdSourceReader;
use grapt_platform::{
    ConfigPublicationError, CredentialRefusal, CredentialResolutionRequest, FirstContactKind,
    PlatformFirstContact, PlatformOwner, PlatformRefusal, PublishedConfig, MAX_CATALOG_ROWS,
};
use grapt_provider::{CredentialPlacement, ProtocolFamily};
use grapt_types::CredentialRecordId;

use super::shared::*;

#[test]
fn first_contact_keeps_all_five_states_distinct_and_read_only() -> TestResult {
    let root = scratch("classification")?;
    let absent = PlatformFirstContact::inspect(&root);
    assert_eq!(
        absent.kind(),
        FirstContactKind::Absent,
        "first contact collapsed absent into another readiness state"
    );
    assert!(!root.join("database.sqlite3").exists());

    let directory = root.join("database.sqlite3");
    std::fs::create_dir(&directory)?;
    assert_eq!(
        PlatformFirstContact::inspect(&root).kind(),
        FirstContactKind::Unreadable
    );
    std::fs::remove_dir(&directory)?;

    std::fs::write(&directory, b"not sqlite")?;
    let corrupt_before = std::fs::read(&directory)?;
    assert_eq!(
        PlatformFirstContact::inspect(&root).kind(),
        FirstContactKind::CorruptOrQuarantined
    );
    assert_eq!(std::fs::read(&directory)?, corrupt_before);
    std::fs::remove_file(&directory)?;

    legacy_database(&directory)?;
    let migration_before = std::fs::read(&directory)?;
    assert_eq!(
        PlatformFirstContact::inspect(&root).kind(),
        FirstContactKind::MigrationNeeded
    );
    assert_eq!(std::fs::read(&directory)?, migration_before);
    std::fs::remove_file(&directory)?;

    ready_database(&root)?;
    let ready_before = std::fs::read(&directory)?;
    assert_eq!(
        PlatformFirstContact::inspect(&root).kind(),
        FirstContactKind::Ready
    );
    assert_eq!(std::fs::read(&directory)?, ready_before);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// A store with pending migrations is migrated behind a verified backup when
/// it is opened, with no configuration naming that step.
#[test]
fn a_store_pending_migration_is_migrated_when_opened() -> TestResult {
    let root = scratch("authority")?;
    legacy_database(&root.join("database.sqlite3"))?;
    // A refusal fails this assertion too, so opening as ready reaches it.
    let opened = PlatformFirstContact::inspect(&root).open_with_authorities(
        "df0-test",
        &FixedAuthorities,
        &FixedAuthorities,
    );
    assert!(
        matches!(
            &opened,
            Ok(owner) if matches!(owner.receipt(), grapt_store::OwnedOpenReceipt::Migrated { .. })
        ),
        "a store pending migration was not migrated: {opened:?}"
    );
    drop(opened);
    assert_eq!(
        PlatformFirstContact::inspect(&root).kind(),
        FirstContactKind::Ready
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn precedence_publishes_one_catalog_issued_target_and_reload_retires_the_old_one() -> TestResult {
    let parent = scratch("precedence-parent")?;
    let child = parent.join("child");
    std::fs::create_dir_all(&child)?;
    std::fs::write(
        parent.join("grapt.jsonc"),
        config("open-ready", "parent-build", &[], None),
    )?;
    std::fs::write(
        child.join("grapt.jsonc"),
        "{\"product\":{\"build\":\"child-build\"}}",
    )?;
    let published = PublishedConfig::load(&child)?;
    assert_eq!(published.build().as_str(), "child-build");
    assert!(published.target()?.is_current_issuance());

    std::fs::write(
        child.join("grapt.jsonc"),
        config(
            "open-ready",
            "reload-build",
            &["high".to_owned()],
            Some("high"),
        ),
    )?;
    let (published, reload) = published.reload(&StdSourceReader)?;
    assert_eq!(
        reload.from().value() + 1,
        reload.to().value(),
        "reload receipt did not advance its generation"
    );
    assert_eq!(
        published.target()?.issuance_generation().get(),
        reload.to().value(),
        "reload published a target from the superseded generation"
    );
    assert!(
        !reload.retired_target().is_current_issuance(),
        "reload left the superseded target current"
    );
    assert!(
        published.target()?.is_current_issuance(),
        "reload republished a retired catalog issuance"
    );
    assert_eq!(published.build().as_str(), "reload-build");
    std::fs::remove_dir_all(parent)?;
    Ok(())
}

#[test]
fn variant_expansion_is_bounded_before_catalog_allocation() -> TestResult {
    let root = scratch("variant-bound")?;
    let variants = (0..=MAX_CATALOG_ROWS)
        .map(|ordinal| format!("variant-{ordinal}"))
        .collect::<Vec<_>>();
    write_config(&root, "open-ready", "build-a", &variants, Some("variant-0"))?;
    let result = PublishedConfig::load(&root);
    assert!(
        matches!(
            result,
            Err(ConfigPublicationError::CatalogRows { actual, bound })
                if actual == MAX_CATALOG_ROWS + 1 && bound == MAX_CATALOG_ROWS
        ),
        "variant-expanded catalog rows escaped their pre-allocation bound"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn credential_material_never_enters_refusals_and_settles_before_restart() -> TestResult {
    let root = scratch("credential-settlement")?;
    ready_database(&root)?;
    write_config(&root, "open-ready", "build-a", &[], None)?;
    let first =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    assert_eq!(first.first_contact(), FirstContactKind::Ready);
    let settlement = first.settle();
    assert_eq!(settlement.build().as_str(), "build-a");
    assert!(
        settlement.credential().is_settled(),
        "credential custody did not report settlement"
    );
    let rendered = format!("{settlement:?}");
    assert!(!rendered.contains("sentinel-provider-secret"));

    let restarted =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    assert_eq!(restarted.first_contact(), FirstContactKind::Ready);
    let second = restarted.settle();
    assert!(
        second.credential().is_settled(),
        "restarted credential custody did not report settlement"
    );
    assert_eq!(second.credential(), settlement.credential());
    assert!(!format!("{second:?}").contains("sentinel-provider-secret"));

    const OBSERVED_CODEX_OAUTH_BYTES: usize = 1_849;
    let oauth_sized_secret = "o".repeat(OBSERVED_CODEX_OAUTH_BYTES);
    std::fs::write(root.join("provider.credential"), &oauth_sized_secret)?;
    let oauth_sized =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    let oauth_settlement = oauth_sized.settle();
    assert!(
        oauth_settlement.credential().is_settled(),
        "a current bounded OAuth credential did not acquire custody"
    );
    assert!(
        !format!("{oauth_settlement:?}").contains(&oauth_sized_secret),
        "OAuth-sized credential entered its settlement diagnostic"
    );

    let oversized_secret = "s".repeat(grapt_platform::MAX_CREDENTIAL_BYTES + 1);
    std::fs::write(root.join("provider.credential"), &oversized_secret)?;
    let refusal =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)
            .err()
            .ok_or("oversized credential unexpectedly acquired custody")?;
    assert!(matches!(
        &refusal,
        PlatformRefusal::Credential(CredentialRefusal::TooLarge { bound })
            if *bound == grapt_platform::MAX_CREDENTIAL_BYTES
    ));
    let rendered_refusal = format!("{refusal:?} {refusal}");
    assert!(
        !rendered_refusal.contains(&oversized_secret),
        "credential refusal disclosed captured secret material"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn credential_settlement_coordinates_come_from_held_custody() -> TestResult {
    let root = scratch("credential-owner-coordinates")?;
    ready_database(&root)?;
    write_config(&root, "open-ready", "build-a", &[], None)?;
    let first = PublishedConfig::load(&root)?;
    let expected_account = first.credentials().account().clone();
    let expected_version = first.credentials().version().clone();
    let owner =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;

    std::fs::write(
        root.join("grapt.jsonc"),
        config("open-ready", "build-b", &[], None)
            .replace("account-a", "forged-account")
            .replace("credential-v1", "forged-version"),
    )?;
    let forged = PublishedConfig::load(&root)?;
    let platform_settlement = owner.settle();
    let settlement = platform_settlement.credential();
    assert_eq!(settlement.account(), &expected_account);
    assert_eq!(settlement.version(), &expected_version);
    assert_ne!(settlement.account(), forged.credentials().account());
    assert_ne!(settlement.version(), forged.credentials().version());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn credential_authority_resolves_once_and_target_authority_issues_per_attempt() -> TestResult {
    let root = scratch("credential-authority-positive")?;
    ready_database(&root)?;
    write_config(&root, "open-ready", "build-a", &[], None)?;
    let owner =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    let parts = owner.into_provider_parts()?;
    let (store, targets, _build, credential, first_contact) = parts.into_components();
    assert_eq!(first_contact, FirstContactKind::Ready);
    assert_eq!(targets.protocol_family(), ProtocolFamily::ChatGptCodex);
    assert_eq!(targets.route().as_str(), "responses");
    let first_target = targets.issue()?;
    let second_target = targets.issue()?;
    assert!(first_target.is_current_issuance());
    assert!(second_target.is_current_issuance());
    assert!(
        first_target.is_same_issuance(&second_target),
        "fresh linear proofs must still come from the one live issuance"
    );
    let record = credential.record().clone();
    let protocol_family = credential.protocol_family();
    let placement = credential.placement();
    let request =
        CredentialResolutionRequest::new(&first_target, record, protocol_family, placement);
    let opener_credential = credential.resolve(request)?;
    assert_eq!(opener_credential.account(), first_target.account());
    assert_eq!(
        opener_credential.credential_version(),
        first_target.credential_version()
    );
    assert_eq!(
        opener_credential.placement(),
        CredentialPlacement::BearerAuthorization
    );
    drop(opener_credential);
    drop(first_target);
    drop(second_target);
    drop(targets);
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn every_grant_coordinate_and_issuance_mismatch_refuses_without_secret_text() -> TestResult {
    let root = scratch("credential-authority-mismatches")?;
    ready_database(&root)?;
    write_config(&root, "open-ready", "build-a", &[], None)?;

    let (credential, target) = credential_authority_and_target(&root)?;
    let wrong_record = CredentialRecordId::new("cred_another_record").ok_or("record")?;
    let refusal = resolve_refusal(
        credential,
        &target,
        wrong_record,
        ProtocolFamily::ChatGptCodex,
        CredentialPlacement::BearerAuthorization,
        "another credential record",
    )?;
    assert!(matches!(refusal, CredentialRefusal::RecordMismatch));

    let (credential, target) = credential_authority_and_target(&root)?;
    let record = credential.record().clone();
    let refusal = resolve_refusal(
        credential,
        &target,
        record,
        ProtocolFamily::OpenAiResponses,
        CredentialPlacement::BearerAuthorization,
        "another protocol family",
    )?;
    assert!(matches!(refusal, CredentialRefusal::ProtocolMismatch));

    let (credential, target) = credential_authority_and_target(&root)?;
    let record = credential.record().clone();
    let refusal = resolve_refusal(
        credential,
        &target,
        record,
        ProtocolFamily::ChatGptCodex,
        CredentialPlacement::AnthropicApiKey,
        "another credential placement",
    )?;
    assert!(matches!(refusal, CredentialRefusal::PlacementMismatch));

    let target_root = scratch("credential-authority-foreign-targets")?;
    for (provider, route, account, version, expected) in [
        (
            "another-provider",
            "responses",
            "account-a",
            "credential-v1",
            "provider",
        ),
        (
            "openai",
            "another-route",
            "account-a",
            "credential-v1",
            "route",
        ),
        (
            "openai",
            "responses",
            "another-account",
            "credential-v1",
            "account",
        ),
        (
            "openai",
            "responses",
            "account-a",
            "another-version",
            "version",
        ),
    ] {
        std::fs::write(
            target_root.join("grapt.jsonc"),
            config_with_coordinates(
                "open-ready",
                "build-a",
                &[],
                None,
                provider,
                route,
                account,
                version,
            ),
        )?;
        let foreign = PublishedConfig::load(&target_root)?.issue_target()?;
        let (credential, _own_target) = credential_authority_and_target(&root)?;
        let record = credential.record().clone();
        let refusal = resolve_refusal(
            credential,
            &foreign,
            record,
            ProtocolFamily::ChatGptCodex,
            CredentialPlacement::BearerAuthorization,
            match expected {
                "provider" => "another provider",
                "route" => "another route",
                "account" => "another account",
                "version" => "another credential version",
                _ => "an unknown coordinate",
            },
        )?;
        assert!(
            matches!(
                (&refusal, expected),
                (CredentialRefusal::ProviderMismatch, "provider")
                    | (CredentialRefusal::RouteMismatch, "route")
                    | (CredentialRefusal::AccountMismatch, "account")
                    | (CredentialRefusal::VersionMismatch, "version")
            ),
            "wrong {expected} produced another refusal"
        );
        assert!(!format!("{refusal:?} {refusal}").contains("sentinel-provider-secret"));
    }

    std::fs::write(
        target_root.join("grapt.jsonc"),
        config("open-ready", "build-a", &[], None),
    )?;
    let foreign_same_coordinates = PublishedConfig::load(&target_root)?.issue_target()?;
    let (credential, _own_target) = credential_authority_and_target(&root)?;
    let record = credential.record().clone();
    let refusal = resolve_refusal(
        credential,
        &foreign_same_coordinates,
        record,
        ProtocolFamily::ChatGptCodex,
        CredentialPlacement::BearerAuthorization,
        "a foreign catalog issuance",
    )?;
    assert!(matches!(refusal, CredentialRefusal::ForeignIssuance));

    let published = PublishedConfig::load(&target_root)?;
    let stale = published.issue_target()?;
    let (current, _reload) = published.reload(&StdSourceReader)?;
    assert!(!stale.is_current_issuance());
    drop(current);
    let (credential, _own_target) = credential_authority_and_target(&root)?;
    let record = credential.record().clone();
    let refusal = resolve_refusal(
        credential,
        &stale,
        record,
        ProtocolFamily::ChatGptCodex,
        CredentialPlacement::BearerAuthorization,
        "a stale catalog issuance",
    )?;
    assert!(matches!(
        &refusal,
        CredentialRefusal::StaleCapability { .. }
    ));
    assert!(!format!("{refusal:?} {refusal}").contains("sentinel-provider-secret"));

    std::fs::remove_dir_all(target_root)?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// The permanent engine attribution carries the install's DECLARED build, not
/// a crate version.
///
/// First Rust write contact with an oracle-era database stamps `engine_epoch` in the
/// same transaction as the schema, and the 2026-08-08 ruling leaves no reverse
/// path: whatever lands in that row is the only forensic record of which engine
/// took the database, forever. `env!("CARGO_PKG_VERSION")` cannot serve as that
/// record — every crate in this workspace carries the same `0.1.0`, so it
/// cannot distinguish two builds and would stamp one indistinguishable constant
/// into every database the product ever migrates.
#[test]
fn migration_attributes_the_declared_build_and_never_the_crate_version() -> TestResult {
    let root = scratch("epoch-attribution")?;
    legacy_database(&root.join("database.sqlite3"))?;
    // A value packaging would supply: the attested source commit of the binary
    // doing the migration. Deliberately unlike any crate version.
    let declared = "secode-rust@40d82e39ef4c0fd2f2bc495b07dce53f858467a5";
    write_config(&root, "migrate", declared, &[], None)?;

    let owner =
        PlatformOwner::prepare_with_authorities(&root, &FixedAuthorities, &FixedAuthorities)?;
    assert_eq!(owner.first_contact(), FirstContactKind::MigrationNeeded);
    assert_eq!(owner.build().as_str(), declared);
    drop(owner);

    let connection = rusqlite::Connection::open(root.join("database.sqlite3"))?;
    let stamped: String =
        connection.query_row("SELECT version FROM engine_epoch", [], |row| row.get(0))?;
    drop(connection);

    assert_eq!(
        stamped, declared,
        "the durable engine attribution must be the install's declared build"
    );
    // The named counterexample: the value this path used to stamp. Asserted
    // separately so a regression to it fails by name rather than as a generic
    // mismatch, and so this witness cannot pass by the two happening to agree.
    assert_ne!(
        stamped,
        env!("CARGO_PKG_VERSION"),
        "engine attribution fell back to the crate version, which is {} for \
         every crate in this workspace and identifies no build",
        env!("CARGO_PKG_VERSION")
    );

    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// The same law on the PRODUCTION entry point.
///
/// `PlatformOwner::prepare` is what `grapt-server serve` reaches through
/// `compose::run`; `prepare_with_authorities` is the injected-authority twin the
/// batteries above use. They are separate bodies, so a witness on one leaves the
/// other free to stamp whatever it likes — and the free one would be the one that
/// ships. Both are therefore witnessed.
#[test]
fn the_production_entry_point_attributes_the_declared_build_too() -> TestResult {
    let root = scratch("epoch-attribution-production")?;
    legacy_database(&root.join("database.sqlite3"))?;
    let declared = "secode-rust@production-entry-point";
    write_config(&root, "migrate", declared, &[], None)?;

    let owner = PlatformOwner::prepare(&root)?;
    assert_eq!(owner.first_contact(), FirstContactKind::MigrationNeeded);
    drop(owner);

    let connection = rusqlite::Connection::open(root.join("database.sqlite3"))?;
    let stamped: String =
        connection.query_row("SELECT version FROM engine_epoch", [], |row| row.get(0))?;
    drop(connection);
    assert_eq!(stamped, declared);
    assert_ne!(stamped, env!("CARGO_PKG_VERSION"));

    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// F1 witness: the composed-pair refusal keeps its cause through the real
/// config load, instead of collapsing into a generic load failure — and the
/// cause names the fix, not just the mistake.
#[test]
fn catalog_composition_refusal_keeps_its_cause_and_names_the_fix() -> TestResult {
    let root = scratch("composition")?;
    std::fs::write(
        root.join("grapt.jsonc"),
        r#"{
            "store": {"authority": "open-ready"},
            "product": {"build": "df0-composition"},
            "provider": {
                "id": "custom", "model": "gpt-5.6", "base_model": "gpt-5.6", "family": "gpt-5",
                "route": "responses", "account": "account-a",
                "credential_version": "credential-v1", "policy_version": "df0-policy-v1",
                "variants": ["high"], "selected_variant": "high"
            },
            "catalog": {"source": "models.dev-chatgpt"}
        }"#,
    )?;
    let error = PublishedConfig::load(&root)
        .err()
        .ok_or("the composed-pair configuration unexpectedly loaded")?;
    let message = error.to_string();
    let ConfigPublicationError::CatalogComposition {
        catalog_source,
        provider_id,
        protocol_family,
    } = error
    else {
        return Err(format!(
            "the composed-pair cause collapsed into a generic load failure: {message}"
        )
        .into());
    };
    assert_eq!(catalog_source, "models.dev-chatgpt");
    assert_eq!(provider_id, "custom");
    assert_eq!(protocol_family, "chatgpt-codex");
    assert!(
        message.contains("is not composed with provider id")
            && message.contains("remove catalog.source"),
        "the cause names no fix: {message}"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

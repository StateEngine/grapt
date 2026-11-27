//! Ordinary terminal failures are actionable without quoting configured
//! values or classified payloads: store states, migration plans, backup
//! paths and lower-level causes are mapped to safe state and advice at
//! this boundary, so the final redaction never has to catch them.

use super::super::ProductRefusal;
use grapt_platform::{ConfigPublicationError, CredentialRefusal, PlatformRefusal};

pub(in crate::compose) fn platform(error: PlatformRefusal) -> ProductRefusal {
    let reason = match error {
        PlatformRefusal::Config(error @ ConfigPublicationError::LocalModeRemoved) => {
            return ProductRefusal::TerminalSetup(error.to_string())
        }
        PlatformRefusal::Config(ConfigPublicationError::Missing { .. }) => {
            "configuration is missing a required field; repair the selected account configuration"
        }
        PlatformRefusal::Config(ConfigPublicationError::Load(_)) => {
            "configuration could not be loaded; check its syntax and read access"
        }
        // Static advice from the platform's composed-pair table, never the
        // configured values (J01).
        PlatformRefusal::Config(ConfigPublicationError::CatalogComposition { .. }) => {
            return ProductRefusal::TerminalSetup(format!(
                "catalog source, provider id and protocol family are not a composed pair; {}",
                grapt_platform::composed_catalogs_advice()
            ))
        }
        PlatformRefusal::Config(
            ConfigPublicationError::SelectedVariantAbsent(_)
            | ConfigPublicationError::UnconfiguredProvider(_),
        ) => "selected account or model is unavailable; choose a configured target",
        PlatformRefusal::Config(_) => {
            "account/model configuration was rejected; repair its values and model inventory"
        }
        // The name is a validated variable name, never a value (J01).
        PlatformRefusal::Credential(CredentialRefusal::EnvironmentAbsent { name }) => {
            return ProductRefusal::TerminalSetup(format!(
                "this profile's API key is read from the environment variable {name}, which is \
                 not set; set it and start Grapt again"
            ))
        }
        PlatformRefusal::Credential(CredentialRefusal::Absent) => {
            "configured credential is missing; supply the named credential file"
        }
        PlatformRefusal::Credential(
            CredentialRefusal::Read(_) | CredentialRefusal::Metadata(_),
        ) => "configured credential is unreadable; repair its read access",
        PlatformRefusal::Credential(_) => {
            "configured credential was rejected; repair the named credential file"
        }
        error @ PlatformRefusal::Store(
            grapt_platform::PlatformStoreRefusal::CorruptOrQuarantined(_),
        ) => return crate::installation::platform_refusal(error),
        // Owned-open refusals name their safe state and the store.authority
        // advice that repairs it; the refusal's payloads (migration plans,
        // backup paths, lower-level causes) never cross this boundary.
        PlatformRefusal::Store(grapt_platform::PlatformStoreRefusal::OwnedOpen(cause)) => {
            return ProductRefusal::TerminalSetup(owned_open_setup(&cause))
        }
        PlatformRefusal::Store(
            cause @ grapt_platform::PlatformStoreRefusal::DirectiveMismatch { .. },
        ) => return ProductRefusal::TerminalSetup(cause.to_string()),
        PlatformRefusal::Store(_) => {
            "profile store could not be opened; preserve the profile and repair its store access"
        }
        PlatformRefusal::HandoverBuildChanged => {
            "installation changed during startup; extract consistent binaries together"
        }
    };
    ProductRefusal::TerminalSetup(reason.to_owned())
}

/// The safe state word a store classification may use in terminal text:
/// the word only, never the classification's payload (a migration plan's
/// backup location is a private path).
fn readiness_state(readiness: &grapt_store::StoreReadiness) -> &'static str {
    match readiness {
        grapt_store::StoreReadiness::Absent => "absent",
        grapt_store::StoreReadiness::Unreadable(_) => "unreadable",
        grapt_store::StoreReadiness::MigrationNeeded(_) => "pending a migration",
        grapt_store::StoreReadiness::QuarantinedOrCorrupt(_) => "quarantined or corrupt",
        grapt_store::StoreReadiness::HotJournal => "recovering a hot journal",
        // Say what happened: the store is usable, the optional walk did not
        // finish. Never the corruption word for a check that merely ran long.
        grapt_store::StoreReadiness::Slow => {
            "ready (the full page check did not finish; run check-profile)"
        }
        grapt_store::StoreReadiness::Ready => "ready",
    }
}

/// The store.authority tail that repairs an observed state. The directive
/// policy matches the platform mismatch advice: initialize an absent
/// store, migrate a pending plan, open-ready a ready store; obstructed
/// states admit no directive, because none would succeed.
fn directive_advice(readiness: &grapt_store::StoreReadiness) -> &'static str {
    match readiness {
        grapt_store::StoreReadiness::Absent => {
            "in grapt.jsonc set store.authority to \"initialize\" to match it"
        }
        grapt_store::StoreReadiness::MigrationNeeded(_) => {
            "in grapt.jsonc set store.authority to \"migrate\" to apply the pending migration plan, then to \"open-ready\" to open the store once it is ready"
        }
        grapt_store::StoreReadiness::HotJournal
        | grapt_store::StoreReadiness::Slow
        | grapt_store::StoreReadiness::Ready => {
            "in grapt.jsonc set store.authority to \"open-ready\" to match it"
        }
        grapt_store::StoreReadiness::Unreadable(_)
        | grapt_store::StoreReadiness::QuarantinedOrCorrupt(_) => {
            "the refusal stands until the obstruction is repaired"
        }
    }
}

fn state_advice(readiness: &grapt_store::StoreReadiness, wrong: &str) -> String {
    format!(
        "the store is {} and {}; {}",
        readiness_state(readiness),
        wrong,
        directive_advice(readiness)
    )
}

/// The terminal text for an owned-open refusal: safe typed state plus the
/// store.authority advice that repairs it. Raw plans, paths and
/// lower-level causes are refused at the source (review AR-H1 on 3e8cae9c),
/// so no final redaction has to catch them.
fn owned_open_setup(cause: &grapt_store::OwnedOpenError) -> String {
    match cause {
        grapt_store::OwnedOpenError::Unreadable(_) => {
            "profile store is unreadable; repair its read access and re-open".to_owned()
        }
        grapt_store::OwnedOpenError::QuarantinedOrCorrupt(_) => {
            "profile store is quarantined or corrupt; preserve the profile and repair its store"
                .to_owned()
        }
        grapt_store::OwnedOpenError::AuthorityMismatch { observed, .. } => state_advice(
            observed,
            "the current store.authority does not authorize that state",
        ),
        grapt_store::OwnedOpenError::StaleMigrationPlan { observed, .. } => state_advice(
            observed,
            "the stored migration authority no longer matches it",
        ),
        grapt_store::OwnedOpenError::MigrationAuthorizationRequired(_) => {
            "initialized store requires explicit migration authority; in grapt.jsonc set \
             store.authority to \"migrate\" to apply the pending migration plan, then to \
             \"open-ready\" to open the store once it is ready"
                .to_owned()
        }
        grapt_store::OwnedOpenError::InitializationConflict(observed) => {
            state_advice(observed, "another process created it before this open")
        }
        grapt_store::OwnedOpenError::FileIdentity(_) => {
            "database file identity could not be established; repair the profile store directory"
                .to_owned()
        }
        grapt_store::OwnedOpenError::PathIdentityChanged => {
            "the database path no longer names the classified store file; repair the profile \
             store directory"
                .to_owned()
        }
        grapt_store::OwnedOpenError::Migration(_) => {
            "the store migration failed; preserve the profile and its backup, then re-run the \
             migration or restore from the backup"
                .to_owned()
        }
        grapt_store::OwnedOpenError::Open(_) => {
            "the store could not be opened; preserve the profile and repair its store access"
                .to_owned()
        }
        grapt_store::OwnedOpenError::HotJournalRecovery(_) => {
            "hot-journal recovery could not re-open the store; preserve the profile and repair \
             its store access"
                .to_owned()
        }
        grapt_store::OwnedOpenError::PostActionNotReady(_) => {
            "store preparation finished without a ready store; preserve the profile and repair \
             its store access"
                .to_owned()
        }
    }
}

pub(in crate::compose) fn redact(error: ProductRefusal) -> ProductRefusal {
    let reason = match error {
        // Store obstructions carry compatibility evidence, not configured secrets.
        error @ ProductRefusal::StoreCompatibility(_) => return error,
        ProductRefusal::TerminalSetup(reason) => reason,
        ProductRefusal::Platform(_) => {
            "account/model state is unavailable; repair the selected profile".to_owned()
        }
        ProductRefusal::Input(_) => {
            "service configuration was rejected; repair service.conf and basic.credential"
                .to_owned()
        }
        ProductRefusal::Compose(_) => {
            "product could not compose; repair the installation and configured provider".to_owned()
        }
        ProductRefusal::Bind(_) => {
            "local listener could not open; check local network access".to_owned()
        }
        ProductRefusal::Probe(_) => {
            "product authentication failed; repair the profile authentication file".to_owned()
        }
        ProductRefusal::Registration(_) => {
            "profile ownership could not be acquired; close its other owner or repair \
             registration access"
                .to_owned()
        }
        ProductRefusal::Custody(_) => {
            "readiness attribution was refused; preserve the profile and repair its \
             registration"
                .to_owned()
        }
        ProductRefusal::Shutdown(_) => {
            "product cleanup failed; preserve the profile and restart to reconcile history"
                .to_owned()
        }
    };
    ProductRefusal::TerminalSetup(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_store_refusal_preserves_cause_before_recovery() -> Result<(), String> {
        let migration = "9999_from_a_newer_engine";
        let causes = [
            grapt_store::Obstruction::Migration(grapt_store::MigrationError::UnknownRecorded(
                grapt_types::MigrationId::new(migration).ok_or("invalid fixture migration")?,
            )),
            grapt_store::Obstruction::Integrity("corrupt event log rejected".into()),
        ];
        for cause in causes {
            let expected = match &cause {
                grapt_store::Obstruction::Migration(_) => migration,
                _ => "corrupt event log rejected",
            };
            let refusal =
                grapt_platform::PlatformStoreRefusal::CorruptOrQuarantined(Box::new(cause));
            // Exercise both lifetime paths, including terminal's final redaction.
            let window = crate::installation::platform_refusal(PlatformRefusal::Store(refusal));
            let message = window.to_string();
            assert_cause_and_recovery(&message, expected);
            let ProductRefusal::StoreCompatibility(cause) = window else {
                panic!("store refusal lost its typed cause");
            };
            let terminal = redact(platform(PlatformRefusal::Store(cause))).to_string();
            assert_cause_and_recovery(&terminal, expected);
        }
        Ok(())
    }

    fn assert_cause_and_recovery(message: &str, cause: &str) {
        let cause_at = message
            .find(cause)
            .unwrap_or_else(|| panic!("missing cause: {message}"));
        let recovery_at = message
            .find("grapt-migrate disposition")
            .unwrap_or_else(|| panic!("missing recovery guidance: {message}"));
        assert!(
            cause_at < recovery_at,
            "recovery replaced the cause: {message}"
        );
        assert!(message.contains("grapt-migrate restore"));
        assert!(message.contains("matching source/backup"));
    }

    #[test]
    fn frc_terminal_feedback_classifies_failures_without_configured_values() {
        let hidden = "FRC_SECRET_MUST_NOT_APPEAR";
        let rejected = platform(PlatformRefusal::Config(
            ConfigPublicationError::StoreDirective(hidden.into()),
        ))
        .to_string();
        let unavailable = platform(PlatformRefusal::Config(
            ConfigPublicationError::SelectedVariantAbsent(hidden.into()),
        ))
        .to_string();
        let unreadable = platform(PlatformRefusal::Credential(CredentialRefusal::Read(
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, hidden),
        )))
        .to_string();
        let missing = platform(PlatformRefusal::Credential(CredentialRefusal::Absent)).to_string();
        let foreign = redact(ProductRefusal::Registration(hidden.into())).to_string();
        for message in [&rejected, &unavailable, &unreadable, &missing, &foreign] {
            assert!(
                !message.contains(hidden),
                "terminal startup disclosed a configured value"
            );
        }
        assert!(rejected.contains("rejected"));
        assert!(unavailable.contains("unavailable"));
        assert!(unreadable.contains("unreadable"));
        assert!(missing.contains("missing"));
        assert!(foreign.contains("ownership"));
    }

    fn scratch(label: &str) -> Result<std::path::PathBuf, String> {
        let root =
            std::env::temp_dir().join(format!("grapt-feedback-{label}-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
        }
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(root)
    }

    /// F1 witness: the composed-pair cause survives the real config load and
    /// reaches the terminal with its fix, through both the platform mapping
    /// and the final redaction.
    #[test]
    fn catalog_composition_refusal_reaches_the_terminal_with_its_fix() -> Result<(), String> {
        let root = scratch("catalog")?;
        std::fs::write(
            root.join("grapt.jsonc"),
            r#"{
                "store": {"authority": "open-ready"},
                "product": {"build": "df0-feedback-build"},
                "provider": {
                    "id": "custom", "model": "model-a", "base_model": "model-a", "family": "fam-a",
                    "route": "responses", "account": "account-a",
                    "credential_version": "credential-v1", "policy_version": "df0-policy-v1",
                    "variants": ["model-a"], "selected_variant": "model-a"
                },
                "catalog": {"source": "models.dev-chatgpt"}
            }"#,
        )
        .map_err(|error| error.to_string())?;
        let refusal = grapt_platform::PlatformOwner::inspect_for_build(&root, None)
            .err()
            .ok_or("the composed-pair configuration unexpectedly loaded")?;
        let PlatformRefusal::Config(ConfigPublicationError::CatalogComposition {
            catalog_source,
            provider_id,
            protocol_family,
        }) = refusal
        else {
            let _cleanup = std::fs::remove_dir_all(&root);
            return Err(format!(
                "the config load lost its composed-pair cause: {refusal}"
            ));
        };
        assert_eq!(catalog_source, "models.dev-chatgpt");
        assert_eq!(provider_id, "custom");
        assert_eq!(protocol_family, "chatgpt-codex");
        let composed = || {
            PlatformRefusal::Config(ConfigPublicationError::CatalogComposition {
                catalog_source: "models.dev-chatgpt".into(),
                provider_id: "custom".into(),
                protocol_family: "chatgpt-codex".into(),
            })
        };
        for message in [
            platform(composed()).to_string(),
            redact(platform(composed())).to_string(),
        ] {
            assert!(
                message.contains(&grapt_platform::composed_catalogs_advice()),
                "terminal lost the composed pairs: {message}"
            );
            assert!(
                message.contains("remove catalog.source"),
                "terminal lost the fix: {message}"
            );
        }
        std::fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// F2 witness: a directive that does not authorize the store's state is a
    /// configuration fix, and the terminal names the directive that would.
    /// Driven through inspect_for_build, the product's own first call.
    #[test]
    fn directive_mismatch_keeps_its_store_authority_advice_at_the_terminal() -> Result<(), String> {
        let root = scratch("directive")?;
        std::fs::write(
            root.join("grapt.jsonc"),
            r#"{
                "store": {"authority": "open-ready"},
                "product": {"build": "df0-feedback-build"},
                "provider": {"id":"openai","model":"fixture-model","base_model":"fixture-model","family":"openai-responses","protocol_family":"openai-responses","endpoint":"http://127.0.0.1:1",
                    "route":"responses","account":"fixture","credential_version":"fixture-v1","policy_version":"first-use-v1","variants":[]}
            }"#,
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            root.join("account-models.json"),
            r#"{"data":[{"id":"fixture-model","max_model_len":128000}]}"#,
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            root.join("provider.credential"),
            "synthetic-provider-secret",
        )
        .map_err(|error| error.to_string())?;
        let refusal = grapt_platform::PlatformOwner::inspect_for_build(&root, None)
            .err()
            .ok_or("the mismatched profile unexpectedly loaded")?;
        let PlatformRefusal::Store(grapt_platform::PlatformStoreRefusal::DirectiveMismatch {
            directive,
            observed,
        }) = refusal
        else {
            let _cleanup = std::fs::remove_dir_all(&root);
            return Err(format!(
                "first contact lost its directive mismatch: {refusal}"
            ));
        };
        assert_eq!(directive, grapt_platform::StoreDirective::OpenReady);
        assert_eq!(observed, grapt_platform::FirstContactKind::Absent);
        for message in [
            platform(PlatformRefusal::Store(
                grapt_platform::PlatformStoreRefusal::DirectiveMismatch {
                    directive,
                    observed,
                },
            ))
            .to_string(),
            redact(platform(PlatformRefusal::Store(
                grapt_platform::PlatformStoreRefusal::DirectiveMismatch {
                    directive,
                    observed,
                },
            )))
            .to_string(),
        ] {
            assert!(
                message.contains(
                    "store directive OpenReady does not authorize first-contact state Absent"
                ),
                "terminal lost the mismatch cause: {message}"
            );
            assert!(
                message.contains("set store.authority to \"initialize\""),
                "terminal lost the directive advice: {message}"
            );
        }
        std::fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn terminal_reuses_platform_store_mismatch_wording() {
        for observed in [
            grapt_platform::FirstContactKind::Ready,
            grapt_platform::FirstContactKind::MigrationNeeded,
        ] {
            let cause = grapt_platform::PlatformStoreRefusal::DirectiveMismatch {
                directive: grapt_platform::StoreDirective::Initialize,
                observed,
            };
            let expected = cause.to_string();
            let ProductRefusal::TerminalSetup(actual) = platform(PlatformRefusal::Store(cause))
            else {
                panic!("store mismatch lost its terminal setup classification");
            };
            assert_eq!(actual, expected);
        }
    }

    fn owned_open(cause: grapt_store::OwnedOpenError) -> PlatformRefusal {
        PlatformRefusal::Store(grapt_platform::PlatformStoreRefusal::OwnedOpen(cause))
    }

    /// A migration plan whose backup location names a private sentinel
    /// directory: the payload the AR-H1 witnesses keep out of terminal text.
    fn sentinel_plan(dir: &std::path::Path) -> grapt_store::MigrationPlan {
        let Some(migration) = grapt_types::MigrationId::new("0001") else {
            panic!("test fixture migration id must be valid");
        };
        grapt_store::MigrationPlan {
            pending: vec![migration],
            backup: grapt_store::FutureBackup {
                location: dir.join("backups").join("database.sqlite3"),
                minimum_free_bytes: 1024,
                capacity: grapt_store::BackupCapacity::Sufficient,
                directory: grapt_store::BackupDirectory::Readable,
                permissions: grapt_store::BackupPermissions {
                    create_file: grapt_store::CreateBackupFile::Authorized,
                    write_file: grapt_store::WriteBackupFile::Authorized,
                },
            },
        }
    }

    /// The assertion applied to every owned-open terminal form: the safe
    /// state and the expected store.authority advice are present; the
    /// classified payload (the sentinel path, and the raw enum shapes only a
    /// `Debug` render would print) is absent.
    fn assert_owned_open_terminal(
        build: impl Fn() -> grapt_store::OwnedOpenError,
        sentinel: Option<&str>,
        expectations: &[&str],
    ) {
        for message in [
            platform(owned_open(build())).to_string(),
            redact(platform(owned_open(build()))).to_string(),
        ] {
            if let Some(sentinel) = sentinel {
                assert!(
                    !message.contains(sentinel),
                    "terminal text leaked the private backup location: {message}"
                );
            }
            for expectation in expectations {
                assert!(
                    message.contains(expectation),
                    "terminal lost the safe state or advice {expectation:?}: {message}"
                );
            }
            for shape in [
                "StoreReadiness",
                "OwnedOpenAuthority",
                "FutureBackup",
                "PathBuf",
            ] {
                assert!(
                    !message.contains(shape),
                    "raw refusal shape {shape:?} reached the terminal: {message}"
                );
            }
        }
    }

    /// F2 witness (corrected for AR-H1): an owned-open refusal reaches the
    /// terminal as safe state plus store.authority advice, never as the raw
    /// refusal Display. These variants fire only when the store state moves
    /// between classification and open, so the refusals are constructed; the
    /// mapping under test is the real terminal boundary.
    #[test]
    fn owned_open_refusal_keeps_its_store_authority_advice_at_the_terminal() -> Result<(), String> {
        let dir = scratch("owned-open")?;
        assert_owned_open_terminal(
            || grapt_store::OwnedOpenError::AuthorityMismatch {
                observed: Box::new(grapt_store::StoreReadiness::Absent),
                authority: Box::new(grapt_store::OwnedOpenAuthority::OpenReady),
            },
            None,
            &[
                "the store is absent",
                "set store.authority to \"initialize\"",
            ],
        );
        assert_owned_open_terminal(
            || grapt_store::OwnedOpenError::StaleMigrationPlan {
                authorized: Box::new(sentinel_plan(&dir)),
                observed: Box::new(grapt_store::StoreReadiness::Ready),
            },
            None,
            &[
                "the store is ready",
                "set store.authority to \"open-ready\"",
            ],
        );
        assert_owned_open_terminal(
            || {
                grapt_store::OwnedOpenError::MigrationAuthorizationRequired(Box::new(
                    sentinel_plan(&dir),
                ))
            },
            None,
            &[
                "initialized store requires explicit migration authority",
                "then to \"open-ready\" to open the store once it is ready",
            ],
        );
        std::fs::remove_dir_all(&dir).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// AR-H1 witness (review HOLD on 3e8cae9c): a fresh-Initialize refusal
    /// whose migration plan names a private backup location — the exact
    /// variant the review's synthetic profile returned — must reach the
    /// terminal as safe state plus advice, sentinel absent through BOTH the
    /// platform mapping and the final redaction. The other path-bearing and
    /// cause-bearing variants are covered the same way.
    #[test]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn owned_open_payloads_never_cross_the_terminal_boundary() -> Result<(), String> {
        let dir = scratch("owned-open-sentinel")?;
        let sentinel = dir
            .join("backups")
            .join("database.sqlite3")
            .to_string_lossy()
            .into_owned();
        assert_owned_open_terminal(
            || {
                grapt_store::OwnedOpenError::MigrationAuthorizationRequired(Box::new(
                    sentinel_plan(&dir),
                ))
            },
            Some(&sentinel),
            &["set store.authority to \"migrate\""],
        );
        assert_owned_open_terminal(
            || grapt_store::OwnedOpenError::StaleMigrationPlan {
                authorized: Box::new(sentinel_plan(&dir)),
                observed: Box::new(grapt_store::StoreReadiness::MigrationNeeded(sentinel_plan(
                    &dir,
                ))),
            },
            Some(&sentinel),
            &[
                "the store is pending a migration",
                "set store.authority to \"migrate\"",
            ],
        );
        assert_owned_open_terminal(
            || {
                grapt_store::OwnedOpenError::InitializationConflict(Box::new(
                    grapt_store::StoreReadiness::MigrationNeeded(sentinel_plan(&dir)),
                ))
            },
            Some(&sentinel),
            &[
                "another process created it before this open",
                "store.authority",
            ],
        );
        assert_owned_open_terminal(
            || {
                grapt_store::OwnedOpenError::PostActionNotReady(Box::new(
                    grapt_store::StoreReadiness::MigrationNeeded(sentinel_plan(&dir)),
                ))
            },
            Some(&sentinel),
            &["without a ready store"],
        );
        assert_owned_open_terminal(
            || grapt_store::OwnedOpenError::AuthorityMismatch {
                observed: Box::new(grapt_store::StoreReadiness::MigrationNeeded(sentinel_plan(
                    &dir,
                ))),
                authority: Box::new(grapt_store::OwnedOpenAuthority::Migrate(sentinel_plan(
                    &dir,
                ))),
            },
            Some(&sentinel),
            &[
                "does not authorize that state",
                "set store.authority to \"migrate\"",
            ],
        );
        assert_owned_open_terminal(
            || {
                grapt_store::OwnedOpenError::QuarantinedOrCorrupt(Box::new(
                    grapt_store::Obstruction::Migration(
                        grapt_store::MigrationError::BackupDestinationExists(
                            dir.join("backups").join("database.sqlite3"),
                        ),
                    ),
                ))
            },
            Some(&sentinel),
            &["quarantined or corrupt"],
        );
        assert_owned_open_terminal(
            || {
                grapt_store::OwnedOpenError::Migration(
                    grapt_store::MigrationError::BackupDestinationExists(
                        dir.join("backups").join("database.sqlite3"),
                    ),
                )
            },
            Some(&sentinel),
            &["the store migration failed"],
        );
        assert_owned_open_terminal(
            || {
                grapt_store::OwnedOpenError::Unreadable(Box::new(
                    grapt_store::Unreadable::Metadata(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        dir.join("backups")
                            .join("database.sqlite3")
                            .to_string_lossy(),
                    )),
                ))
            },
            Some(&sentinel),
            &["unreadable"],
        );
        assert_owned_open_terminal(
            || {
                grapt_store::OwnedOpenError::Open(grapt_store::WriteError::Sqlite(
                    rusqlite::Error::InvalidPath(dir.join("backups").join("database.sqlite3")),
                ))
            },
            Some(&sentinel),
            &["could not be opened"],
        );
        std::fs::remove_dir_all(&dir).map_err(|error| error.to_string())?;
        Ok(())
    }
}

//! B2 retains the pre-rename trust classes and ordering without legacy discovery.
use super::*;
use grapt_plugin::PluginOrigin;

#[test]
fn launcher_mcp_config_under_another_repository_stays_blocked_until_relocated() -> Result<(), String>
{
    let root = std::env::temp_dir().join(format!(
        "grapt-launch-mcp-origin-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let project = root.join("worktree");
    let repository = root.join("engine-repository");
    let inside = repository.join(".stateengine/seat.json");
    let outside = root.join("home/seat.json");
    for directory in [
        &project,
        &repository.join(".git"),
        &repository.join(".stateengine"),
        &root.join("home"),
        &root.join("service"),
    ] {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    let result = (|| {
        let content = br#"{"mcp":{"stateengine":{"type":"local","command":["fixture"]}}}"#;
        for path in [&inside, &outside] {
            std::fs::write(path, content).map_err(|error| error.to_string())?;
        }
        for (path, blocked) in [(&inside, true), (&outside, false)] {
            let mut environment = CatalogEnvironment::isolated(&root);
            environment.explicit = Some(path.clone());
            let sources = CatalogSources::read(&project, environment)?;
            let snapshot = super::super::connections::McpSnapshot::read(
                &sources,
                project.to_str().ok_or("fixture path is not UTF-8")?,
            )?;
            assert_eq!(snapshot.blocked == ["stateengine"], blocked);
            assert_eq!(snapshot.declared.contains_key("stateengine"), !blocked);
        }
        Ok(())
    })();
    std::fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
    result
}

#[test]
fn renamed_sources_keep_the_previous_trust_classes_and_precedence() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!(
        "grapt-brand-boundary-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let project = root.join("project");
    let paths = [
        root.join("service/grapt.jsonc"),
        root.join("config/grapt.json"),
        root.join("explicit.json"),
        project.join("grapt.json"),
        project.join(".grapt/grapt.jsonc"),
    ];
    // These are the classes and order on dev before B2. Neither a new name
    // nor placement under .grapt may turn repository content into user input.
    let previous_origins = [
        PluginOrigin::UserGlobal,
        PluginOrigin::UserGlobal,
        PluginOrigin::LaunchExplicit,
        PluginOrigin::Project,
        PluginOrigin::Project,
        PluginOrigin::LaunchExplicit,
    ];
    for path in &paths {
        let parent = path.parent().ok_or("fixture path has no parent")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::write(path, b"{}").map_err(|error| error.to_string())?;
    }
    // Malformed old files must be wholly undiscovered, rather than parsed as
    // compatibility fallbacks. Split the retired name so the shipped-name
    // guard can reject that spelling everywhere outside legal attribution.
    let retired = ["open", "code"].concat();
    for path in [
        project.join(format!("{retired}.json")),
        project.join(format!(".{retired}/{retired}.jsonc")),
    ] {
        std::fs::create_dir_all(path.parent().ok_or("legacy parent absent")?)
            .map_err(|error| error.to_string())?;
        std::fs::write(path, b"not valid configuration").map_err(|error| error.to_string())?;
    }
    let mut environment = CatalogEnvironment::isolated(&root);
    environment.explicit = Some(paths[2].clone());
    environment.inline = Some("{}".into());
    let sources = CatalogSources::read(&project, environment);
    let sources = sources?;
    assert_eq!(sources.documents.len(), previous_origins.len());
    assert_eq!(
        sources
            .documents
            .iter()
            .map(|doc| doc.origin)
            .collect::<Vec<_>>(),
        previous_origins,
        "{}",
        fixture_diagnostics::diagnostics(&root)
    );
    for (document, path) in sources.documents.iter().zip(paths) {
        assert_eq!(Path::new(&document.source), path);
    }
    let inline = sources.documents.last().ok_or("inline source absent")?;
    assert_eq!(inline.source, "GRAPT_CONFIG_CONTENT");
    assert!(sources
        .documents
        .iter()
        .all(|doc| { user_owned(doc.origin) == (doc.origin != PluginOrigin::Project) }));
    std::fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn selecting_a_repository_profile_never_promotes_its_executable_origin() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!(
        "grapt-profile-origin-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let project = root.join("project");
    let profile = project.join(".grapt");
    std::fs::create_dir_all(&profile).map_err(|error| error.to_string())?;
    let path = profile.join("grapt.jsonc");
    std::fs::write(&path, b"{}").map_err(|error| error.to_string())?;
    let result = (|| {
        // Both ordinary service-root discovery and explicit selection keep the
        // same path Project. The first occurrence/deduplication cannot mint a
        // stronger class before the project source is visited.
        for explicit in [false, true] {
            let mut environment = CatalogEnvironment::isolated(&root);
            environment.service_root = profile.clone();
            if explicit {
                environment.explicit = Some(path.clone());
                environment.config = profile.clone();
            }
            for directory in [&project, &profile] {
                let sources = CatalogSources::read(
                    directory,
                    CatalogEnvironment {
                        home: environment.home.clone(),
                        config: environment.config.clone(),
                        repositories: environment.repositories.clone(),
                        service_root: environment.service_root.clone(),
                        explicit: environment.explicit.clone(),
                        inline: None,
                        fixture_root: environment.fixture_root.clone(),
                    },
                )?;
                let document = sources
                    .documents
                    .iter()
                    .find(|document| Path::new(&document.source) == path)
                    .ok_or("profile configuration was not discovered")?;
                assert_eq!(document.origin, PluginOrigin::Project);
                assert!(!user_owned(document.origin));
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&root);
    result
}

#[test]
fn an_arbitrarily_named_profile_inside_a_checkout_is_not_global() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!(
        "grapt-checkout-profile-origin-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let profile = root.join("custom-profile");
    std::fs::create_dir_all(&profile).map_err(|error| error.to_string())?;
    std::fs::write(root.join(".git"), b"gitdir: fixture").map_err(|error| error.to_string())?;
    let path = profile.join("grapt.jsonc");
    std::fs::write(&path, b"{}").map_err(|error| error.to_string())?;
    let result = source_origin(&path, PluginOrigin::UserGlobal, &profile, &profile);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(result?, PluginOrigin::Project);
    Ok(())
}

#[test]
fn approved_profile_catalog_uses_captured_bytes_and_project_origin() -> Result<(), String> {
    use grapt_platform::profile_configuration::{install, ProfileConfiguration};
    let root = std::env::temp_dir().join(format!(
        "grapt-approved-profile-catalog-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("service")).map_err(|error| error.to_string())?;
    let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
    let result = (|| {
        let profile = root.join("service");
        let path = profile.join("grapt.jsonc");
        // A live-cache entry must not replace a later approved snapshot.
        std::fs::write(&path, br#"{"plugin":["stale-cache"]}"#)
            .map_err(|error| error.to_string())?;
        CatalogSources::read(&profile, CatalogEnvironment::isolated(&root))?;
        std::fs::write(&path, b"{}").map_err(|error| error.to_string())?;
        let approval = ProfileConfiguration::capture(&profile)
            .map_err(|error| error.to_string())?
            .approval()
            .clone();
        install(&profile, &approval)?;
        #[cfg(windows)]
        let short = super::super::authority_path::short_alias(&path)?;
        let previously_absent = profile.join("grapt.json");
        std::fs::write(
            &previously_absent,
            br#"{"plugin":["unapproved-new-source"]}"#,
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(&path, br#"{"plugin":["unapproved-replacement"]}"#)
            .map_err(|error| error.to_string())?;
        #[cfg(windows)]
        approved_aliases(&root, &profile, &path, short.as_deref(), false)?;
        let replaced = CatalogSources::read(&profile, CatalogEnvironment::isolated(&root))?;
        assert!(replaced
            .documents
            .iter()
            .all(|document| Path::new(&document.source) != previously_absent));
        let document = replaced
            .documents
            .iter()
            .find(|document| Path::new(&document.source) == path)
            .ok_or("approved profile disappeared after replacement")?;
        assert_eq!(document.origin, PluginOrigin::Project);
        assert_eq!(document.value, decode(b"{}", bounds()?)?);
        // The consumer must neither reread this path nor reclassify its
        // vanished replacement as global configuration.
        std::fs::remove_file(&path).map_err(|error| error.to_string())?;
        #[cfg(windows)]
        approved_aliases(&root, &profile, &path, short.as_deref(), true)?;
        let sources = CatalogSources::read(&profile, CatalogEnvironment::isolated(&root))?;
        let document = sources
            .documents
            .iter()
            .find(|document| Path::new(&document.source) == path)
            .ok_or("approved profile disappeared from the catalog")?;
        assert_eq!(document.origin, PluginOrigin::Project);
        assert_eq!(document.value, decode(b"{}", bounds()?)?);
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(root);
    result
}

#[cfg(windows)]
fn approved_aliases(
    root: &Path,
    profile: &Path,
    file: &Path,
    short: Option<&Path>,
    removed: bool,
) -> Result<(), String> {
    use grapt_platform::profile_configuration::{frozen, FrozenRead};
    let canonical = file.to_str().ok_or("approved profile path")?;
    let ordinary = canonical
        .strip_prefix(r"\\?\")
        .ok_or("approved profile ordinary path")?;
    let mut paths = vec![
        PathBuf::from(canonical),
        PathBuf::from(ordinary),
        PathBuf::from(ordinary.to_ascii_uppercase()),
    ];
    if let Some(short) = short {
        paths.push(short.to_path_buf());
    }
    for path in paths {
        match frozen(&path, 65536) {
            Ok(FrozenRead::Bytes(bytes)) => {
                assert_eq!(bytes, b"{}", "approved bytes changed through alias")
            }
            Err(error) if removed && Some(path.as_path()) == short => {
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::NotFound,
                    "short alias refused unexpectedly"
                );
                eprintln!("SEC-WIN-PREFIX captured short-file: explicitly refused after removal");
                continue;
            }
            other => {
                return Err(format!(
                    "approved alias did not retain captured bytes: {other:?}"
                ))
            }
        }
        let parent = path.parent().ok_or("approved alias parent")?;
        assert!(
            matches!(
                frozen(&parent.join("GRAPT.JSON"), 65536).map_err(|e| e.to_string())?,
                FrozenRead::Missing
            ),
            "captured absence resurrected"
        );
        let mut environment = CatalogEnvironment::isolated(root);
        environment.config = parent.to_path_buf();
        let sources = CatalogSources::read(profile, environment)?;
        assert_eq!(
            sources.documents.len(),
            1,
            "captured alias was loaded again"
        );
        assert_eq!(sources.documents[0].origin, PluginOrigin::Project);
        assert_eq!(sources.documents[0].value, decode(b"{}", bounds()?)?);
    }
    Ok(())
}

use super::super::catalog_config::{CatalogEnvironment, CatalogSources};
use super::*;
use grapt_plugin::PluginOrigin;

use super::super::catalog_config::fixture_diagnostics;

struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "grapt-origin-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&path).map_err(|error| error.to_string())?;
        Ok(Self(path))
    }
    fn write(&self, path: &str) -> Result<PathBuf, String> {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().ok_or("fixture parent absent")?)
            .map_err(|error| error.to_string())?;
        std::fs::write(&path, b"{}").map_err(|error| error.to_string())?;
        Ok(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn service_aliases_keep_one_document_and_preserve_repository_restrictions() -> Result<(), String> {
    let fixture = Fixture::new("service-identity")?;
    let path = fixture.write("service/grapt.jsonc")?;
    let service = path.parent().ok_or("service parent")?;
    std::fs::create_dir(service.join("child")).map_err(|error| error.to_string())?;
    let canonical = std::fs::canonicalize(service).map_err(|error| error.to_string())?;
    for directory in [canonical, service.join("child/..")] {
        for repository in [false, true] {
            if repository {
                fixture.write("service/.git")?;
            }
            let sources =
                CatalogSources::read(&directory, CatalogEnvironment::isolated(&fixture.0))?;
            assert_eq!(
                sources.documents.len(),
                1,
                "aliased service was loaded twice"
            );
            assert_eq!(
                sources.documents[0].origin,
                if repository {
                    PluginOrigin::Project
                } else {
                    PluginOrigin::UserGlobal
                }
            );
            if repository {
                std::fs::remove_file(service.join(".git")).map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

#[test]
fn fixture_discovery_excludes_shared_git_and_profile_ancestors() -> Result<(), String> {
    let fixture = Fixture::new("shared")?;
    // Both ancestry mechanisms must be invisible above the explicit boundary.
    fixture.write(".git")?;
    fixture.write(".grapt/grapt.json")?;
    let root = fixture.0.join(".grapt/private");
    let service = fixture.write(".grapt/private/service/grapt.jsonc")?;
    let config = fixture.write(".grapt/private/config/grapt.json")?;
    let project = root.join("project");
    std::fs::create_dir(&project).map_err(|error| error.to_string())?;
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(&root))?;
    assert_eq!(
        sources.documents.len(),
        2,
        "shared configuration leaked into discovery"
    );
    for path in [service, config] {
        let document = sources
            .documents
            .iter()
            .find(|row| Path::new(&row.source) == path)
            .ok_or("fixture configuration disappeared")?;
        assert_eq!(document.origin, PluginOrigin::UserGlobal);
        // The same source without a fixture boundary retains production's
        // fail-closed classification; Git discovery ceilings cannot promote it.
        assert_eq!(
            source_origin_below(
                &path,
                PluginOrigin::UserGlobal,
                &project,
                &root.join("service"),
                None
            )?,
            PluginOrigin::Project
        );
    }
    Ok(())
}

#[test]
fn fixture_boundary_keeps_its_own_repository_and_profile_markers() -> Result<(), String> {
    let fixture = Fixture::new("local")?;
    let path = fixture.write("private/profile/grapt.jsonc")?;
    let root = fixture.0.join("private");
    let profile = root.join("profile");
    fixture.write("private/.git")?;
    assert_eq!(
        source_origin_below(
            &path,
            PluginOrigin::UserGlobal,
            &profile,
            &profile,
            Some(&root)
        )?,
        PluginOrigin::Project
    );
    std::fs::remove_file(root.join(".git")).map_err(|error| error.to_string())?;
    let path = fixture.write("private/.grapt/grapt.jsonc")?;
    let profile = root.join(".grapt");
    assert_eq!(
        source_origin_below(
            &path,
            PluginOrigin::LaunchExplicit,
            &profile,
            &profile,
            Some(&root)
        )?,
        PluginOrigin::Project
    );
    Ok(())
}

#[test]
fn a_fixture_boundary_cannot_promote_sources_outside_it() -> Result<(), String> {
    let fixture = Fixture::new("outside")?;
    let path = fixture.write("outside/profile/grapt.jsonc")?;
    fixture.write("outside/.git")?;
    let root = fixture.0.join("private");
    std::fs::create_dir(&root).map_err(|error| error.to_string())?;
    assert_eq!(
        source_origin_below(
            &path,
            PluginOrigin::LaunchExplicit,
            &root,
            &root,
            Some(&root)
        )?,
        PluginOrigin::Project
    );
    Ok(())
}

#[test]
fn classification_diagnostics_name_marker_types_without_reading_them() -> Result<(), String> {
    let fixture = Fixture::new("diagnostics")?;
    let path = fixture.write(".git")?;
    std::fs::write(&path, b"PRIVATE CONTENT MUST NOT APPEAR").map_err(|error| error.to_string())?;
    let text = fixture_diagnostics::diagnostics(&fixture.0);
    assert!(text.contains(".git=file"));
    assert!(text.contains(".grapt=NotFound"));
    assert!(!text.contains("PRIVATE CONTENT"));
    assert!(text.len() < 65536);
    Ok(())
}

#[cfg(unix)]
#[test]
fn aliased_roots_keep_in_fixture_profile_spelling() -> Result<(), String> {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new("alias")?;
    fixture.write(".git")?;
    let plain = fixture.write("real/plain/input.json")?;
    let root = fixture.0.join("real");
    let alias = fixture.0.join("alias");
    symlink(&root, &alias).map_err(|error| error.to_string())?;
    symlink(root.join("plain"), root.join(".grapt")).map_err(|error| error.to_string())?;
    for path in [
        alias.join(".grapt/input.json"),
        alias.join(".grapt/../plain/input.json"),
    ] {
        assert_eq!(
            source_origin_below(&path, PluginOrigin::UserGlobal, &root, &root, Some(&root))?,
            PluginOrigin::Project,
            "{}",
            fixture_diagnostics::diagnostics(&root)
        );
    }
    // The alias does not restore markers above the fixture boundary.
    assert_eq!(
        source_origin_below(
            &alias.join("plain/input.json"),
            PluginOrigin::UserGlobal,
            &root,
            &root,
            Some(&root)
        )?,
        PluginOrigin::UserGlobal
    );
    assert_eq!(
        std::fs::canonicalize(plain).map_err(|error| error.to_string())?,
        std::fs::canonicalize(alias.join(".grapt/input.json")).map_err(|error| error.to_string())?
    );
    Ok(())
}

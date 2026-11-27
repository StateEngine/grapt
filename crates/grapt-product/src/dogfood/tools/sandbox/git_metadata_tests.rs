use super::*;
use crate::dogfood::catalog_config::CatalogEnvironment;

// Exact write-grant handles are implemented only on these two platforms.
#[cfg(any(target_os = "linux", windows))]
#[test]
fn explicit_git_protection_narrows_layers_without_changing_false_or_default_authority(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-git-policy-fold-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let workspace = root.join("workspace");
    let config = root.join("config");
    std::fs::create_dir(&workspace)?;
    std::fs::create_dir(&config)?;
    std::fs::create_dir(workspace.join(".git"))?;
    let index = workspace.join(".git/index");
    std::fs::write(&index, "index")?;
    std::fs::write(workspace.join(".git/config"), "")?;
    let read = || {
        Plan::read(
            &workspace,
            &CatalogSources::read(&workspace, CatalogEnvironment::isolated(&root))?,
        )
    };
    assert!(read()?.is_none());
    std::fs::write(
        config.join("grapt.json"),
        br#"{"sandbox":{"git_metadata_read_only":false}}"#,
    )?;
    assert!(read()?.is_none(), "false silently enabled containment");
    std::fs::write(
        config.join("grapt.json"),
        br#"{"sandbox":{"enabled":true,"git_metadata_read_only":false}}"#,
    )?;
    let legacy = read()?.ok_or("legacy isolation absent")?.policy();
    assert!(
        grapt_sandbox::WriteTarget::resolve(&legacy, &index).is_ok(),
        "false removed existing Git write authority"
    );
    assert!(grapt_sandbox::WriteTarget::resolve(&legacy, &workspace.join(".git/config")).is_err());
    std::fs::write(
        config.join("grapt.json"),
        br#"{"sandbox":{"enabled":false,"git_metadata_read_only":true}}"#,
    )?;
    std::fs::write(
        workspace.join("grapt.json"),
        br#"{"sandbox":{"enabled":false,"git_metadata_read_only":false}}"#,
    )?;
    let narrowed = read()?
        .ok_or("explicit Git protection did not enable isolation")?
        .policy();
    assert!(
        grapt_sandbox::WriteTarget::resolve(&narrowed, &index).is_err(),
        "later false removed Git protection"
    );
    let ordinary = workspace.join("ordinary");
    std::fs::write(&ordinary, "workspace")?;
    assert!(
        grapt_sandbox::WriteTarget::resolve(&narrowed, &ordinary).is_ok(),
        "Git protection removed ordinary workspace authority"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn unresolved_or_unreadable_git_pointers_refuse_required_protection(
) -> Result<(), Box<dyn std::error::Error>> {
    let root =
        std::env::temp_dir().join(format!("grapt-git-policy-pointer-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::create_dir(root.join("config"))?;
    std::fs::write(
        root.join("config/grapt.json"),
        br#"{"sandbox":{"git_metadata_read_only":true}}"#,
    )?;
    let read = || {
        Plan::read(
            &workspace,
            &CatalogSources::read(&workspace, CatalogEnvironment::isolated(&root))?,
        )
    };
    for pointer in ["gitdir: missing", "gitdir: ", "not a pointer"] {
        std::fs::write(workspace.join(".git"), pointer)?;
        assert!(
            read().is_err(),
            "required Git metadata accepted unresolved pointer {pointer:?}"
        );
    }
    std::fs::remove_file(workspace.join(".git"))?;
    std::fs::create_dir(workspace.join(".git"))?;
    std::fs::create_dir(workspace.join(".git/commondir"))?;
    assert!(
        read().is_err(),
        "unreadable common-directory pointer was treated as absent"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn strict_git_pointer_validation_preserves_default_and_false_compatibility(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-git-policy-legacy-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::create_dir(root.join("config"))?;
    std::fs::write(workspace.join(".git"), "gitdir: \n")?;
    for flag in [None, Some(false), Some(true)] {
        let mut settings = serde_json::json!({"sandbox":{"enabled":true}});
        if let Some(flag) = flag {
            settings["sandbox"]["git_metadata_read_only"] = flag.into();
        }
        std::fs::write(
            root.join("config/grapt.json"),
            serde_json::to_vec(&settings)?,
        )?;
        let sources = CatalogSources::read(&workspace, CatalogEnvironment::isolated(&root))?;
        let policy = Plan::read(&workspace, &sources);
        if flag == Some(true) {
            assert!(
                policy.is_err(),
                "strict protection admitted an empty directory pointer"
            );
        } else {
            assert!(
                policy?.is_some(),
                "legacy pointer behavior changed without explicit protection"
            );
        }
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

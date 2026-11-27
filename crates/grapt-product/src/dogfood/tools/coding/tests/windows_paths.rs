use super::*;

#[test]
fn ordinary_dos_path_matches_canonical_workspace() -> Result<(), String> {
    let given = std::env::current_dir().map_err(|error| error.to_string())?;
    let root = std::fs::canonicalize(given).map_err(|error| error.to_string())?;
    let ordinary = root
        .to_str()
        .ok_or("root is not UTF-8")?
        .strip_prefix(r"\\?\")
        .ok_or("root is not verbatim")?;
    println!("root={} requested={ordinary}", root.display());
    assert_eq!(
        relative_path(&root, ordinary)
            .map_err(|error| format!("ordinary DOS workspace refused: {error}"))?,
        Path::new("")
    );
    Ok(())
}

#[test]
fn equivalent_drive_and_unc_prefixes_keep_components() -> Result<(), String> {
    let scratch = Scratch::new("windows-prefix-equivalence")?;
    let verbatim = scratch.0.to_str().ok_or("root is not UTF-8")?;
    let ordinary = grapt_types::path::windows_comparison_key(verbatim);
    let ordinary_backslash = ordinary.replace('/', "\\");
    let ordinary_root = Path::new(&ordinary_backslash);
    for (root, requested) in [
        (
            scratch.0.as_path(),
            format!("{ordinary_backslash}\\src\\new.rs"),
        ),
        (scratch.0.as_path(), format!("{ordinary}/src/new.rs")),
        (ordinary_root, format!("{verbatim}\\src\\new.rs")),
    ] {
        assert_eq!(relative_path(root, &requested)?, Path::new(r"src\new.rs"));
    }
    // UNC spelling preserves components through the shared comparison owner.
    // Native workspace authority requires a real directory identity.
    for (root, requested) in [
        (r"\\?\UNC\host\share\repo", r"\\host\share\repo\src\new.rs"),
        (r"\\host\share\repo", r"\\?\UNC\host\share\repo\src\new.rs"),
    ] {
        let root = grapt_types::path::windows_comparison_key(root);
        let requested = grapt_types::path::windows_comparison_key(requested);
        assert_eq!(
            requested.strip_prefix(&format!("{root}/")),
            Some("src/new.rs")
        );
    }
    Ok(())
}

#[test]
fn windows_prefix_aliases_do_not_expand_authority() -> Result<(), String> {
    let scratch = Scratch::new("windows-prefix-refusals")?;
    let root = scratch.0.as_path();
    let ordinary =
        grapt_types::path::windows_comparison_key(root.to_str().ok_or("root is not UTF-8")?);
    assert_eq!(
        relative_path(root, &format!("{ordinary}/control"))?,
        Path::new("control")
    );
    let other_drive = if ordinary.starts_with("c:/") {
        "D"
    } else {
        "C"
    };
    for path in [
        format!("{other_drive}:/grapt-outside-root"),
        format!("{ordinary}-other"),
        format!("{ordinary}/../escape"),
        format!("{ordinary}/src/../file"),
        r"C:work\repo".to_owned(),
        r"\work\repo".to_owned(),
        format!("{ordinary}/file:stream"),
        format!("{ordinary}/file."),
        format!("{ordinary}/file "),
    ] {
        assert!(relative_path(root, &path).is_err(), "accepted {path}");
    }
    let prefix = grapt_types::path::windows_comparison_key(r"\\?\UNC\host\share\repo");
    for path in [
        r"\\other\share\repo",
        r"\\host\other\repo",
        r"\\host\share\repo-other",
    ] {
        let key = grapt_types::path::windows_comparison_key(path);
        assert!(
            key.strip_prefix(&format!("{prefix}/")).is_none(),
            "accepted {path}"
        );
    }
    assert!(relative_path(root, &"x".repeat(PATH_BYTES + 1)).is_err());
    Ok(())
}

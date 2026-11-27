use super::super::tests::Scratch;
use super::*;

#[test]
fn ir_nonempty_profile_created_before_publish_is_preserved() -> Result<(), String> {
    let scratch = Scratch::new("stranger-at-publish")?;
    let target = scratch.0.join("profile");
    let staged = StagedProfile::begin(&target)?;
    fs::write(staged.root().join("ready"), b"IR_STAGED").map_err(|error| error.to_string())?;
    fs::create_dir(&target).map_err(|error| error.to_string())?;
    fs::write(target.join("keep"), b"IR_USER_DATA").map_err(|error| error.to_string())?;
    assert!(staged.publish().is_err());
    assert_eq!(
        fs::read(target.join("keep")).map_err(|error| error.to_string())?,
        b"IR_USER_DATA"
    );
    Ok(())
}

#[test]
fn ir_markerless_half_profile_is_untouched_and_has_move_aside_advice() -> Result<(), String> {
    let scratch = Scratch::new("markerless")?;
    let source = scratch.account()?;
    let target = scratch.0.join("profile");
    fs::create_dir(&target).map_err(|error| error.to_string())?;
    fs::write(target.join("grapt.jsonc"), b"IR_TORN_CONFIG").map_err(|error| error.to_string())?;
    let error = super::super::initialize(&target, &source)
        .err()
        .ok_or("IR markerless half-profile accepted")?;
    assert!(
        error.contains("move it aside")
            && error.contains("rerun")
            && error.contains(&target.display().to_string()),
        "IR unsafe or unhelpful recovery: {error}"
    );
    assert_eq!(
        fs::read(target.join("grapt.jsonc")).map_err(|error| error.to_string())?,
        b"IR_TORN_CONFIG"
    );
    Ok(())
}

#[test]
fn ir_cleanup_is_flat_bounded_and_validates_before_deleting() -> Result<(), String> {
    let scratch = Scratch::new("cleanup-bound")?;
    let root = scratch.0.join("claimed");
    fs::create_dir(&root).map_err(|error| error.to_string())?;
    let over_cap = 32 + 4 * grapt_connections::store::CONNECTION_CAP + 1;
    for index in 0..over_cap {
        fs::write(root.join(format!("file-{index}")), b"IR_KEEP")
            .map_err(|error| error.to_string())?;
    }
    assert!(clean_claimed(&root).is_err());
    for index in 0..over_cap {
        assert_eq!(
            fs::read(root.join(format!("file-{index}"))).map_err(|error| error.to_string())?,
            b"IR_KEEP"
        );
    }
    Ok(())
}

#[test]
fn ir_nonregular_claimed_entry_preserves_siblings() -> Result<(), String> {
    let scratch = Scratch::new("cleanup-directory")?;
    let root = scratch.0.join("claimed");
    fs::create_dir(&root).map_err(|error| error.to_string())?;
    fs::create_dir(root.join("nested")).map_err(|error| error.to_string())?;
    fs::write(root.join("keep"), b"IR_KEEP").map_err(|error| error.to_string())?;
    assert!(clean_claimed(&root).is_err());
    assert!(root.join("nested").is_dir());
    assert_eq!(
        fs::read(root.join("keep")).map_err(|error| error.to_string())?,
        b"IR_KEEP"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn ir_linked_staging_and_entries_never_delete_their_targets() -> Result<(), String> {
    let scratch = Scratch::new("cleanup-link")?;
    let foreign = scratch.0.join("foreign");
    fs::create_dir(&foreign).map_err(|error| error.to_string())?;
    fs::write(foreign.join("keep"), b"IR_FOREIGN").map_err(|error| error.to_string())?;
    let stage = scratch.0.join("claimed");
    std::os::unix::fs::symlink(&foreign, &stage).map_err(|error| error.to_string())?;
    assert!(clean_claimed(&stage).is_err());
    fs::remove_file(&stage).map_err(|error| error.to_string())?;
    fs::create_dir(&stage).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(foreign.join("keep"), stage.join("linked"))
        .map_err(|error| error.to_string())?;
    assert!(clean_claimed(&stage).is_err());
    assert_eq!(
        fs::read(foreign.join("keep")).map_err(|error| error.to_string())?,
        b"IR_FOREIGN"
    );
    Ok(())
}

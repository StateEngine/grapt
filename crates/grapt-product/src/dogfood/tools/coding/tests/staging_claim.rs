//! Reclamation owns only an unlocked regular sibling with the same open identity.
use super::super::patch::{atomic_replace, stage};
use super::*;

#[test]
fn file01_a_live_stage_is_not_reclaimed_by_another_writer() -> Result<(), String> {
    let scratch = Scratch::new("file01-live-stage")?;
    let target = scratch.0.join("source.txt");
    std::fs::write(&target, b"old").map_err(|error| error.to_string())?;
    let first = stage(&scratch.0, &target, b"first", "write", "write")?;
    let refusal = atomic_replace(&scratch.0, &target, b"second", "write", "write", &|| Ok(()))
        .err()
        .ok_or("live stage was reclaimed")?;
    assert!(
        refusal.contains("another writer owns this stage"),
        "{refusal}"
    );
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"old"
    );
    assert_eq!(
        std::fs::read(first.temp()).map_err(|error| error.to_string())?,
        b"first"
    );
    first.commit(&|| Ok(()))?;
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"first"
    );
    Ok(())
}

#[test]
fn file01_an_unlocked_legacy_stage_is_reclaimed_without_using_its_bytes() -> Result<(), String> {
    let scratch = Scratch::new("file01-legacy-stage")?;
    let target = scratch.0.join("source.txt");
    let temp = scratch.0.join(".source.txt.grapt-write.tmp");
    std::fs::write(&target, b"old").map_err(|error| error.to_string())?;
    std::fs::write(&temp, b"untrusted abandoned bytes").map_err(|error| error.to_string())?;
    atomic_replace(&scratch.0, &target, b"new", "write", "write", &|| Ok(()))?;
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"new"
    );
    assert!(!temp.try_exists().map_err(|error| error.to_string())?);
    Ok(())
}

#[test]
fn file01_a_rebound_stage_is_neither_published_nor_deleted() -> Result<(), String> {
    let scratch = Scratch::new("file01-rebound-stage")?;
    let target = scratch.0.join("source.txt");
    std::fs::write(&target, b"old").map_err(|error| error.to_string())?;
    let staged = stage(&scratch.0, &target, b"planned", "write", "write")?;
    let temp = staged.temp().to_owned();
    let displaced = scratch.0.join("displaced-stage");
    std::fs::rename(&temp, &displaced).map_err(|error| error.to_string())?;
    std::fs::write(&temp, b"foreign replacement").map_err(|error| error.to_string())?;
    let refusal = staged
        .commit(&|| Ok(()))
        .err()
        .ok_or("foreign stage was published")?;
    assert!(refusal.contains("stage identity changed"), "{refusal}");
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"old"
    );
    assert_eq!(
        std::fs::read(&temp).map_err(|error| error.to_string())?,
        b"foreign replacement"
    );
    assert_eq!(
        std::fs::read(&displaced).map_err(|error| error.to_string())?,
        b"planned"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn file01_staging_links_and_nonregular_entries_are_refused_without_following_them(
) -> Result<(), String> {
    let scratch = Scratch::new("file01-linked-stage")?;
    let outside = Scratch::new("file01-outside-stage")?;
    let target = scratch.0.join("source.txt");
    let external = outside.0.join("untouched.txt");
    let temp = scratch.0.join(".source.txt.grapt-write.tmp");
    std::fs::write(&target, b"old").map_err(|error| error.to_string())?;
    std::fs::write(&external, b"outside").map_err(|error| error.to_string())?;
    for destination in [&external, &outside.0.join("absent")] {
        std::os::unix::fs::symlink(destination, &temp).map_err(|error| error.to_string())?;
        let refusal = atomic_replace(&scratch.0, &target, b"new", "write", "write", &|| Ok(()))
            .err()
            .ok_or("stage link was followed")?;
        assert!(refusal.contains("stage is not a regular file"), "{refusal}");
        assert!(std::fs::symlink_metadata(&temp)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_symlink());
        std::fs::remove_file(&temp).map_err(|error| error.to_string())?;
    }
    std::fs::create_dir(&temp).map_err(|error| error.to_string())?;
    assert!(atomic_replace(&scratch.0, &target, b"new", "write", "write", &|| Ok(())).is_err());
    assert!(temp.is_dir());
    assert_eq!(
        std::fs::read(&external).map_err(|error| error.to_string())?,
        b"outside"
    );
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"old"
    );
    Ok(())
}

#[test]
fn file01_reclaiming_a_hardlinked_stage_does_not_change_its_other_name() -> Result<(), String> {
    let scratch = Scratch::new("file01-hardlinked-stage")?;
    let target = scratch.0.join("source.txt");
    let other = scratch.0.join("untouched.txt");
    let temp = scratch.0.join(".source.txt.grapt-write.tmp");
    std::fs::write(&target, b"old").map_err(|error| error.to_string())?;
    std::fs::write(&other, b"outside").map_err(|error| error.to_string())?;
    std::fs::hard_link(&other, &temp).map_err(|error| error.to_string())?;
    atomic_replace(&scratch.0, &target, b"new", "write", "write", &|| Ok(()))?;
    assert_eq!(
        std::fs::read(&other).map_err(|error| error.to_string())?,
        b"outside"
    );
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"new"
    );
    Ok(())
}

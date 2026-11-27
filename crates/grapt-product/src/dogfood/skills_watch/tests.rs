use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn new_edited_and_recreated_skill_folders_invalidate_their_location(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = Scratch(std::env::temp_dir().join(format!(
        "grapt-skill-watch-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir(&root.0)?;
    std::fs::create_dir_all(root.0.join("project"))?;
    std::fs::create_dir_all(root.0.join("home"))?;
    let project = root.0.join("project/.agents/skills");
    let user = root.0.join("home/.agents/skills");
    let directory = root.0.join("project");
    let directory = directory.to_str().ok_or("fixture directory encoding")?;
    let watch = Watch::new();
    watch.track(directory, vec![project.clone(), user.clone()])?;
    async fn changed(watch: &Watch, directory: &str) -> Result<(), Box<dyn std::error::Error>> {
        let changes = tokio::time::timeout(Duration::from_secs(5), watch.changed()).await?;
        let selected = LocationRef::new(directory, None)?.canonical_id();
        assert!(changes
            .iter()
            .any(|location| location.canonical_id() == selected));
        // Drain a late SDK event before the next independent filesystem edit.
        for _ in 0..4 {
            if tokio::time::timeout(Duration::from_millis(400), watch.changed())
                .await
                .is_err()
            {
                return Ok(());
            }
        }
        Err("skill watches did not settle after reading the catalog".into())
    }
    for folder in [&project, &user] {
        std::fs::create_dir_all(folder.join("sample"))?;
        let file = folder.join("sample/SKILL.md");
        std::fs::write(&file, "---\nname: sample\ndescription: First\n---\nBody\n")?;
        changed(&watch, directory).await?;
        std::fs::write(&file, "---\nname: sample\ndescription: Edited\n---\nBody\n")?;
        changed(&watch, directory).await?;
        std::fs::remove_dir_all(folder)?;
        changed(&watch, directory).await?;
        std::fs::create_dir_all(folder.join("again"))?;
        std::fs::write(
            folder.join("again/SKILL.md"),
            "---\nname: again\n---\nBody\n",
        )?;
        changed(&watch, directory).await?;
        // Replacement at the same path within one debounce must not leave a
        // native watch attached to the deleted directory.
        std::fs::remove_dir_all(folder)?;
        std::fs::create_dir_all(folder.join("replaced"))?;
        let replacement = folder.join("replaced/SKILL.md");
        std::fs::write(&replacement, "---\nname: replaced\n---\nFirst\n")?;
        changed(&watch, directory).await?;
        std::fs::write(&replacement, "---\nname: replaced\n---\nEdited\n")?;
        changed(&watch, directory).await?;
    }
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn directories_created_during_registration_remain_watched_after_setup(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = Scratch(std::env::temp_dir().join(format!(
        "grapt-skill-registration-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir(&root.0)?;
    let skills = root.0.join("skills");
    std::fs::create_dir(&skills)?;
    let canonical = std::fs::canonicalize(&skills)?;
    let directory = root.0.to_str().ok_or("fixture directory encoding")?;
    let location = LocationRef::new(directory, None)?;
    let watch = Watch::new();
    let selected = location.canonical_id();
    // Ancestor watches also see unrelated activity. It must not schedule a
    // re-arm that turns the next catalog read into another invalidation.
    let signals = Signals::default();
    lock(&signals.scopes).insert(selected.clone(), (location.clone(), vec![skills.clone()]));
    signals.observe(Ok(Event::new(EventKind::Create(
        notify::event::CreateKind::Folder,
    ))
    .add_path(root.0.join("unrelated"))));
    assert!(lock(&signals.dirty).is_empty());
    assert!(!signals.rewatch.load(Ordering::Acquire));
    signals.observe(Ok(Event::new(EventKind::Modify(
        notify::event::ModifyKind::Data(notify::event::DataChange::Content),
    ))
    .add_path(skills.join("SKILL.md"))));
    assert!(lock(&signals.dirty).contains(&selected));
    assert!(!signals.rewatch.load(Ordering::Acquire));
    lock(&signals.dirty).clear();
    signals.observe(Ok(Event::new(EventKind::Modify(
        notify::event::ModifyKind::Any,
    ))
    .add_path(root.0.clone())));
    assert!(lock(&signals.dirty).is_empty());
    assert!(!signals.rewatch.load(Ordering::Acquire));
    signals.observe(Ok(Event::new(EventKind::Modify(
        notify::event::ModifyKind::Any,
    ))
    .add_path(skills.join("SKILL.md"))));
    assert!(lock(&signals.dirty).contains(&selected));
    assert!(!signals.rewatch.load(Ordering::Acquire));
    signals.observe(Ok(Event::new(EventKind::Remove(
        notify::event::RemoveKind::Folder,
    ))
    .add_path(skills.clone())));
    assert!(signals.rewatch.load(Ordering::Acquire));
    lock(&watch.state).sources.insert(
        selected.clone(),
        (location, BTreeSet::from([skills.clone()])),
    );
    for (rewatch, name) in [(false, "during-start"), (true, "during-rearm")] {
        let child = skills.join(name);
        let mut created = false;
        watch.reconcile_with(&mut lock(&watch.state), rewatch, &mut |path| {
            if path == canonical {
                std::fs::create_dir(&child).map_err(|error| error.to_string())?;
                std::fs::write(child.join("SKILL.md"), "Before")
                    .map_err(|error| error.to_string())?;
                created = true;
            }
            Ok(())
        })?;
        assert!(created, "registration control did not run");
        assert!(
            lock(&watch.state)
                .registered
                .contains(&std::fs::canonicalize(&child)?),
            "directory created during registration escaped the completed watch plan"
        );
        // Settle creation events before the independent edit. The registration
        // assertion above cannot be repaired by asynchronous reconciliation.
        let mut settled = false;
        for _ in 0..4 {
            if tokio::time::timeout(Duration::from_millis(600), watch.changed())
                .await
                .is_err()
            {
                settled = true;
                break;
            }
        }
        assert!(settled, "registration events did not settle");
        std::fs::write(child.join("SKILL.md"), "Edited after setup")?;
        let changed = tokio::time::timeout(Duration::from_secs(5), watch.changed()).await?;
        assert!(changed.iter().any(|value| value.canonical_id() == selected));
    }
    // A registration can restart the FSEvents stream for every location.
    // Edit the established root while another scope is being registered.
    let other = root.0.join("other");
    std::fs::create_dir(&other)?;
    let other_canonical = std::fs::canonicalize(&other)?;
    let other_location = LocationRef::new(other.to_str().ok_or("fixture path encoding")?, None)?;
    let other_id = other_location.canonical_id();
    lock(&watch.state).sources.insert(
        other_id.clone(),
        (other_location, BTreeSet::from([other.clone()])),
    );
    let mut edited = false;
    watch.reconcile_with(&mut lock(&watch.state), false, &mut |path| {
        if path == other_canonical {
            std::fs::write(
                skills.join("during-start/SKILL.md"),
                "Edited during registration",
            )
            .map_err(|error| error.to_string())?;
            edited = true;
        }
        Ok(())
    })?;
    assert!(edited, "cross-location registration control did not run");
    assert!(lock(&watch.signals.dirty).contains(&selected));
    assert!(lock(&watch.signals.dirty).contains(&other_id));

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let unsupported = skills.join(std::ffi::OsString::from_vec(vec![b'x', 0xff]));
        match std::fs::create_dir(&unsupported) {
            Ok(()) => {
                let error = watch.reconcile(&mut lock(&watch.state), false);
                assert!(error.is_err_and(|error| error.contains("Unicode watch paths")));
            }
            // APFS rejects a non-UTF-8 name before the watcher can encounter it.
            #[cfg(target_os = "macos")]
            Err(error) if error.raw_os_error() == Some(92) => {
                assert!(!unsupported.exists());
            }
            Err(error) => return Err(error.into()),
        }
        assert!(!lock(&watch.state).registered.contains(&unsupported));
    }
    Ok(())
}

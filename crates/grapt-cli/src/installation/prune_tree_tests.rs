//! Source-only prune inspection counterexamples; no version deletion.

use super::{Result, Scratch};
use crate::installation::prune_tree;

#[test]
fn prune_inspection_refuses_links_and_file_aliases_without_touching_outside() -> Result {
    for alias in [false, true] {
        let root = Scratch::new()?;
        let tree = root.0.join("version");
        std::fs::create_dir(&tree)?;
        let outside = root.0.join("outside");
        std::fs::write(&outside, b"outside bytes")?;
        if alias {
            std::fs::hard_link(&outside, tree.join("linked"))?;
        } else {
            std::os::unix::fs::symlink(&outside, tree.join("linked"))?;
        }
        assert!(prune_tree::inspect(&tree, &mut 0).is_err());
        assert_eq!(std::fs::read(&outside)?, b"outside bytes");
        assert!(tree.join("linked").exists());
    }
    Ok(())
}

#[test]
fn prune_inspection_counts_empty_directories_and_keeps_postorder() -> Result {
    let root = Scratch::new()?;
    let tree = root.0.join("version");
    std::fs::create_dir_all(tree.join("bin/empty"))?;
    std::fs::write(tree.join("bin/program"), b"binary")?;
    let mut total = 0;
    let inspected = prune_tree::inspect(&tree, &mut total)?;
    assert_eq!(total, 4);
    assert_eq!(inspected.files, [tree.join("bin/program")]);
    assert_eq!(
        inspected.directories,
        [tree.join("bin/empty"), tree.join("bin"), tree.clone()]
    );
    assert!(tree.join("bin/program").exists());
    Ok(())
}

#[test]
fn prune_inspection_refuses_excessive_depth_and_a_spent_global_bound() -> Result {
    let root = Scratch::new()?;
    let tree = root.0.join("version");
    let mut nested = tree.clone();
    for _ in 0..=crate::installation::bounds::TREE_DEPTH {
        nested.push("nested");
    }
    std::fs::create_dir_all(&nested)?;
    assert!(prune_tree::inspect(&tree, &mut 0).is_err());
    let mut exhausted = crate::installation::bounds::VERSION_ENTRIES;
    assert!(prune_tree::inspect(&tree, &mut exhausted).is_err());
    assert_eq!(exhausted, crate::installation::bounds::VERSION_ENTRIES);
    assert!(nested.exists());
    Ok(())
}

#[test]
fn prune_inspection_refuses_more_than_the_per_version_inventory() -> Result {
    let root = Scratch::new()?;
    let tree = root.0.join("version");
    std::fs::create_dir(&tree)?;
    let bound = crate::installation::bounds::TREE_ENTRIES;
    for value in 0..bound {
        std::fs::write(tree.join(format!("entry-{value}")), b"keep")?;
    }
    assert!(prune_tree::inspect(&tree, &mut 0).is_err());
    assert!(tree.join(format!("entry-{}", bound - 1)).exists());
    Ok(())
}

#[test]
fn prune_inspection_refuses_nonregular_entries() -> Result {
    let root = Scratch::new()?;
    let tree = root.0.join("version");
    std::fs::create_dir(&tree)?;
    let socket = tree.join("socket");
    let _listener = std::os::unix::net::UnixListener::bind(&socket)?;
    assert!(prune_tree::inspect(&tree, &mut 0).is_err());
    assert!(socket.exists());
    Ok(())
}

#[test]
fn prune_rechecks_reject_substitution_and_content_changes() -> Result {
    let root = Scratch::new()?;
    let tree = root.0.join("version");
    std::fs::create_dir(&tree)?;
    let path = tree.join("payload");
    std::fs::write(&path, b"original")?;
    let inspected = prune_tree::inspect(&tree, &mut 0)?;
    std::fs::rename(&path, root.0.join("retained-original"))?;
    std::fs::write(&path, b"original")?;
    assert!(inspected.recheck().is_err());
    let inspected = prune_tree::inspect(&tree, &mut 0)?;
    std::fs::write(&path, b"modified bytes")?;
    assert!(inspected.recheck().is_err());
    assert!(path.exists());
    Ok(())
}

#[test]
fn prune_tree_descriptions_do_not_hold_their_own_removal_handles() -> Result {
    let root = Scratch::new()?;
    let tree = root.0.join("version");
    std::fs::create_dir(&tree)?;
    std::fs::write(tree.join("payload"), b"original")?;
    let mut inspected = prune_tree::inspect(&tree, &mut 0)?;
    let quarantine = root.0.join("quarantine");
    std::fs::rename(&tree, &quarantine)?;
    inspected.relocate(&tree, &quarantine)?;
    inspected.recheck()?;
    assert_eq!(inspected.bytes()?, 8);
    Ok(())
}

//! No-follow filesystem acquisition; callers retain directory guards through I/O.
//! Apache-2.0, Copyright 2025 OpenAI.
//! Modified for exclusive creation and validation of the retained parent chain.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(super) use unix::Parent;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::Parent;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn replacing_a_bound_parent_with_a_link_refuses_creation(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = crate::testfs::ScratchDir::new("import-parent-swap")?;
        let parent = root.join("parent");
        let outside = root.join("outside");
        std::fs::create_dir(&parent)?;
        std::fs::create_dir(&outside)?;
        let acquired = Parent::open(&parent.join("imported.md"), false)?;
        std::fs::rename(&parent, root.join("retired"))?;
        std::os::unix::fs::symlink(&outside, &parent)?;
        assert!(acquired.create_file().is_err());
        assert!(!outside.join("imported.md").exists());
        assert!(!root.join("retired/imported.md").exists());
        Ok(())
    }
}

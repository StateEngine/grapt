//! Process-loss/barrier witnesses, compiled only with the loopback testkit.
//! Arming happens after startup on an isolated fixture root. Each flag is
//! consumed once; every wait has a fixed poll bound and needs explicit release.

use std::{path::PathBuf, time::Duration};

#[derive(Debug)]
pub(super) struct Fault {
    root: PathBuf,
}

impl Fault {
    pub(super) fn prepare() -> Option<Self> {
        let root = std::env::var_os("GRAPT_PERMISSION_VIEW_BARRIER").map(PathBuf::from)?;
        if consume(&root, "before-commit") {
            wait_release(&root);
            return None;
        }
        root.join("after-commit.arm")
            .is_file()
            .then_some(Self { root })
    }

    pub(super) fn after_commit(self) {
        if consume(&self.root, "after-commit") {
            wait_release(&self.root);
        }
    }
}

fn consume(root: &std::path::Path, phase: &str) -> bool {
    match std::fs::rename(
        root.join(format!("{phase}.arm")),
        root.join(format!("{phase}.entered")),
    ) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => panic!("permission view fault flag failed: {error}"),
    }
}

fn wait_release(root: &std::path::Path) {
    for _ in 0..2000 {
        if root.join("release").is_file() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("permission view fixture barrier exceeded 2000 polls");
}

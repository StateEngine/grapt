//! A replacement FIFO must not strand commit, cleanup or identity inspection.
use super::*;
use crate::dogfood::tools::tests::Scratch;
use std::os::unix::fs::FileTypeExt;
use std::process::{Child, Command, Stdio};

const CHILD: &str = "dogfood::tools::coding::patch::staging::tests::file01_inspection_child";

struct InspectionChild(Child);

impl Drop for InspectionChild {
    fn drop(&mut self) {
        let _kill = self.0.kill();
        let _wait = self.0.wait();
    }
}

#[test]
fn file01_replaced_stage_inspections_are_bounded() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let mut child = InspectionChild(
        Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .args(["--ignored", "--exact", CHILD, "--nocapture"])
            .env("GRAPT_FILE01_INSPECTION_ROOT", &scratch.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| error.to_string())?,
    );
    for _ in 0..1_000 {
        if let Some(status) = child.0.try_wait().map_err(|error| error.to_string())? {
            assert!(status.success(), "stage inspection child failed: {status}");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err("replacement FIFO blocked stage inspection, commit or Drop".into())
}

fn fifo(path: &Path) -> Result<(), String> {
    let status = Command::new("mkfifo")
        .arg(path)
        .status()
        .map_err(|error| error.to_string())?;
    if !status.success() {
        return Err(format!("mkfifo failed: {status}"));
    }
    Ok(())
}

#[test]
#[ignore = "bounded subprocess helper for file01_replaced_stage_inspections_are_bounded"]
fn file01_inspection_child() -> Result<(), String> {
    let root = PathBuf::from(
        std::env::var_os("GRAPT_FILE01_INSPECTION_ROOT").ok_or("missing inspection root")?,
    );
    let target = root.join("source.txt");
    std::fs::write(&target, b"old").map_err(|error| error.to_string())?;

    // Regular control uses the same open and identity path as the race cases.
    stage(&root, &target, b"control", "write", "control")?.commit(&|| Ok(()))?;
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"control"
    );

    // Metadata can be stale before the next open. A FIFO with no peer must
    // return an error, and its opened handle must never count as a regular file.
    let inspected = root.join("inspected-stage");
    std::fs::write(&inspected, b"observed regular").map_err(|error| error.to_string())?;
    let before = std::fs::symlink_metadata(&inspected).map_err(|error| error.to_string())?;
    assert!(before.is_file());
    std::fs::remove_file(&inspected).map_err(|error| error.to_string())?;
    fifo(&inspected)?;
    let refusal = open_stage(&inspected)
        .err()
        .ok_or("FIFO was accepted as regular")?;
    assert!(
        refusal.to_string().contains("not a regular file"),
        "{refusal}"
    );
    assert!(std::fs::symlink_metadata(&inspected)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_fifo());

    // Swap the stage at the final caller check; both commit and its failed
    // cleanup inspect the new FIFO, and must retain the target and FIFO.
    let staged = stage(&root, &target, b"planned", "write", "commit")?;
    let temp = staged.temp().to_owned();
    let displaced = root.join("displaced-commit");
    let refusal = staged
        .commit(&|| {
            std::fs::rename(&temp, &displaced).map_err(|error| error.to_string())?;
            fifo(&temp)
        })
        .err()
        .ok_or("replacement FIFO was published")?;
    assert!(refusal.contains("not a regular file"), "{refusal}");
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"control"
    );
    assert!(std::fs::symlink_metadata(&temp)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_fifo());
    assert_eq!(
        std::fs::read(&displaced).map_err(|error| error.to_string())?,
        b"planned"
    );

    // An explicit uncommitted Drop also keeps the replacement entry intact.
    let staged = stage(&root, &target, b"drop", "write", "drop")?;
    let temp = staged.temp().to_owned();
    std::fs::rename(&temp, root.join("displaced-drop")).map_err(|error| error.to_string())?;
    fifo(&temp)?;
    drop(staged);
    assert!(std::fs::symlink_metadata(&temp)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_fifo());

    // The shared inspection open must not follow a replacement symlink either.
    let link = root.join("linked-stage");
    std::os::unix::fs::symlink(&target, &link).map_err(|error| error.to_string())?;
    assert!(open_stage(&link).is_err(), "stage symlink was followed");
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"control"
    );
    Ok(())
}

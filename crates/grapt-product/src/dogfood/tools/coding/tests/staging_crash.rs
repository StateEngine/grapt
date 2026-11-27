//! A killed staging owner runs no Drop; the same patch must still be retryable.
use super::*;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::process::{Child, Command, Stdio};

const CHILD: &str = "dogfood::tools::coding::tests::staging_crash::file01_staging_child";

struct StagingChild(Child);

impl Drop for StagingChild {
    fn drop(&mut self) {
        let _kill = self.0.kill();
        let _wait = self.0.wait();
    }
}

#[test]
#[ignore = "subprocess helper, run by file01_a_killed_stage_does_not_block_the_same_patch"]
fn file01_staging_child() -> Result<(), String> {
    let root =
        PathBuf::from(std::env::var_os("GRAPT_FILE01_CHILD_ROOT").ok_or("missing child root")?);
    let staged =
        super::super::patch::stage(&root, &root.join("source.txt"), b"new\n", "patch", "nt-0")?;
    // The parent kills us with the real stage still alive, before publication.
    std::fs::write(
        root.join("stage-prepared"),
        staged.temp().to_str().ok_or("non-UTF-8 stage path")?,
    )
    .map_err(|error| error.to_string())?;
    std::fs::rename(root.join("stage-prepared"), root.join("stage-ready"))
        .map_err(|error| error.to_string())?;
    let mut release = [0];
    std::io::stdin()
        .read_exact(&mut release)
        .map_err(|error| error.to_string())?;
    Err("staging child unexpectedly released instead of killed".into())
}

#[test]
fn file01_a_killed_stage_does_not_block_the_same_patch() -> Result<(), String> {
    let scratch = Scratch::new("file01-killed-stage")?;
    let root = &scratch.0;
    let target = root.join("source.txt");
    std::fs::write(&target, b"old\n").map_err(|error| error.to_string())?;
    let before = format!("{:x}", Sha256::digest(b"old\n"));
    let patch = serde_json::json!({"operations":[{
        "op":"update", "path":"source.txt", "before_sha256":before,
        "old_text":"old", "new_text":"new"
    }]});
    let mut child = StagingChild(
        Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .args(["--ignored", "--exact", CHILD, "--nocapture"])
            .env("GRAPT_FILE01_CHILD_ROOT", root)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| error.to_string())?,
    );
    let ready = root.join("stage-ready");
    for _ in 0..1_000 {
        if ready.try_exists().map_err(|error| error.to_string())? {
            break;
        }
        if let Some(status) = child.0.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("staging child exited before readiness: {status}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let staged_path = std::fs::read_to_string(&ready).map_err(|error| error.to_string())?;
    assert!(
        Path::new(&staged_path).is_file(),
        "child did not stage a file"
    );
    let refusal = call(root, PATCH_TOOL, patch.clone())
        .err()
        .ok_or("active stage was reclaimed")?;
    assert!(
        refusal.contains("another writer owns this stage"),
        "{refusal}"
    );
    assert!(
        Path::new(&staged_path).is_file(),
        "active stage was removed"
    );
    child.0.kill().map_err(|error| error.to_string())?;
    let status = child.0.wait().map_err(|error| error.to_string())?;
    assert!(
        !status.success(),
        "staging child was not forcibly terminated"
    );
    assert!(
        Path::new(&staged_path).is_file(),
        "crash evidence disappeared"
    );
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"old\n"
    );
    call(root, PATCH_TOOL, patch)?;
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"new\n"
    );
    assert!(
        !Path::new(&staged_path).exists(),
        "dead staging file was not reclaimed"
    );
    Ok(())
}

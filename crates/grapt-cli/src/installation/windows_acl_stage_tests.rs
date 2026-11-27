//! Native rows for the install-permission check (WINDOWS-INSTALL-ACL S1-S3):
//! a real update through `commands::run`, with real `icacls` loosening and
//! restoring a fixture installation under LOCALAPPDATA.

use super::*;

/// A disposable installation root under LOCALAPPDATA, or `None` after the
/// named witness ran itself in a bounded child given one (the native runner
/// clears user environment values).
async fn local_root(witness: &str) -> Result<Option<Scratch>> {
    let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        let root = Scratch::new()?;
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args(["--exact", witness, "--test-threads=1", "--nocapture"])
            .env("LOCALAPPDATA", &root.0)
            .env("SystemRoot", system_root())
            .kill_on_drop(true)
            .spawn()?;
        let status =
            match tokio::time::timeout(std::time::Duration::from_secs(60), child.wait()).await {
                Ok(status) => status?,
                Err(_) => {
                    child.start_kill()?;
                    tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await??;
                    return Err(format!("{witness} exceeded its deadline").into());
                }
            };
        return if status.success() {
            Ok(None)
        } else {
            Err(format!("isolated {witness} failed").into())
        };
    };
    io::directory(&local)?;
    let marker = local.join(format!(
        "grapt-acl-stage-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&marker)?;
    Ok(Some(Scratch(marker)))
}

fn update(source: &Path, prefix: &Path) -> Vec<String> {
    vec![
        "update".to_owned(),
        "--from".to_owned(),
        source.to_string_lossy().into_owned(),
        "--prefix".to_owned(),
        prefix.to_string_lossy().into_owned(),
    ]
}

/// The selection records a refused activation must leave byte-identical.
/// Absent is `None`; any other read failure fails the witness (ACL-H3).
fn records(prefix: &Path) -> Result<Vec<Option<Vec<u8>>>> {
    ["state.json", "current", "history.jsonl", "bootstrap.json"]
        .iter()
        .map(|name| match std::fs::read(prefix.join(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("record {name} is unreadable: {error}").into()),
        })
        .collect()
}

fn icacls(arguments: &[&std::ffi::OsStr]) -> Result {
    let status = std::process::Command::new(Path::new(&system_root()).join(r"System32\icacls.exe"))
        .args(arguments)
        .stdout(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("icacls {arguments:?} failed: {status}").into())
    }
}

#[tokio::test]
async fn s1_s2_a_versions_directory_users_can_write_refuses_until_restored() -> Result {
    let Some(local) = local_root(
        "installation::windows_stage_tests::acl::s1_s2_a_versions_directory_users_can_write_refuses_until_restored",
    )
    .await?
    else {
        return Ok(());
    };
    let packages = Scratch::new()?;
    let prefix = local.0.join("installation");
    let a = fixture(&packages.0, A)?;
    let b = fixture(&packages.0, B)?;
    // S1: the inherited LOCALAPPDATA permissions activate.
    super::super::commands::run(&update(&a, &prefix)).await?;
    let versions = prefix.join("versions");
    icacls(&[
        versions.as_os_str(),
        "/grant".as_ref(),
        "*S-1-5-32-545:(OI)(CI)W".as_ref(),
    ])?;
    let before = records(&prefix)?;
    // S2: Users may write versions\, so B is not activated and nothing moves.
    let refused = match super::super::commands::run(&update(&b, &prefix)).await {
        Err(refused) => refused,
        Ok(message) => return Err(format!("activated over a loosened ACL: {message}").into()),
    };
    println!("measured S2 refusal: {refused}");
    assert!(
        refused.contains("grants Users (S-1-5-32-545) write access"),
        "{refused}"
    );
    assert!(refused.contains("/remove:g '*S-1-5-32-545'"), "{refused}");
    assert!(
        refused.ends_with("installation remains unchanged"),
        "{refused}"
    );
    assert_eq!(records(&prefix)?, before);
    assert_eq!(
        transition::state(&prefix)?.ok_or("missing state")?.current,
        A
    );
    // The printed remedy restores it, and the same update then activates.
    icacls(&[
        versions.as_os_str(),
        "/remove:g".as_ref(),
        "*S-1-5-32-545".as_ref(),
    ])?;
    super::super::commands::run(&update(&b, &prefix)).await?;
    assert_eq!(
        transition::state(&prefix)?.ok_or("missing state")?.current,
        B
    );
    Ok(())
}

#[tokio::test]
async fn s3_a_reader_that_cannot_run_refuses_and_publishes_nothing() -> Result {
    let Some(local) = local_root(
        "installation::windows_stage_tests::acl::s3_a_reader_that_cannot_run_refuses_and_publishes_nothing",
    )
    .await?
    else {
        return Ok(());
    };
    let packages = Scratch::new()?;
    let prefix = local.0.join("installation");
    let a = fixture(&packages.0, A)?;
    super::super::windows_acl::READER_FAULT.with(|fault| fault.set(Some("test reader fault")));
    let outcome = super::super::commands::run(&update(&a, &prefix)).await;
    super::super::windows_acl::READER_FAULT.with(|fault| fault.set(None));
    let refused = match outcome {
        Err(refused) => refused,
        Ok(message) => return Err(format!("a reader failure passed: {message}").into()),
    };
    assert_eq!(
        refused,
        "could not verify install permissions (test reader fault); installation remains unchanged"
    );
    assert!(transition::state(&prefix)?.is_none());
    assert!(
        !prefix.join(r"bin\grapt.exe").exists(),
        "the launcher was published"
    );
    assert!(!prefix.join("current").exists(), "a build was selected");
    Ok(())
}

#[tokio::test]
async fn s2b_a_file_inside_the_target_build_users_can_write_refuses_until_restored() -> Result {
    let Some(local) = local_root(
        "installation::windows_stage_tests::acl::s2b_a_file_inside_the_target_build_users_can_write_refuses_until_restored",
    )
    .await?
    else {
        return Ok(());
    };
    let packages = Scratch::new()?;
    let prefix = local.0.join("installation");
    let a = fixture(&packages.0, A)?;
    let b = fixture(&packages.0, B)?;
    super::super::commands::run(&update(&a, &prefix)).await?;
    super::super::commands::run(&update(&b, &prefix)).await?;
    // The review probe shape: only one file of the target build, no directory.
    let file = prefix
        .join("versions")
        .join(A)
        .join("bin")
        .join("grapt-server.exe");
    icacls(&[
        file.as_os_str(),
        "/grant".as_ref(),
        "*S-1-5-32-545:W".as_ref(),
    ])?;
    let before = records(&prefix)?;
    let refused = match super::super::commands::run(&update(&a, &prefix)).await {
        Err(refused) => refused,
        Ok(message) => {
            return Err(format!("activated a build with a writable file: {message}").into())
        }
    };
    println!("measured S2b refusal: {refused}");
    assert!(
        refused.contains("grapt-server.exe grants Users (S-1-5-32-545) write access"),
        "{refused}"
    );
    assert_eq!(records(&prefix)?, before);
    assert_eq!(
        transition::state(&prefix)?.ok_or("missing state")?.current,
        B
    );
    icacls(&[
        file.as_os_str(),
        "/remove:g".as_ref(),
        "*S-1-5-32-545".as_ref(),
    ])?;
    super::super::commands::run(&update(&a, &prefix)).await?;
    assert_eq!(
        transition::state(&prefix)?.ok_or("missing state")?.current,
        A
    );
    Ok(())
}

/// What PowerShell 7 hands a child: its own module directories first.
const PWSH_MODULE_PATH: &str = r"C:\Users\example\Documents\PowerShell\Modules;C:\Program Files\PowerShell\Modules;C:\Program Files\PowerShell\7\Modules;C:\WINDOWS\system32\WindowsPowerShell\v1.0\Modules";

/// ACL-H2: grapt.exe launched from PowerShell 7 inherits its PSModulePath.
/// The whole witness re-runs in a child given that value; install and update
/// must still read owners (meaningful where PowerShell 7 is installed).
#[tokio::test]
async fn s4_a_powershell_7_parent_environment_still_reads_owners() -> Result {
    const WITNESS: &str =
        "installation::windows_stage_tests::acl::s4_a_powershell_7_parent_environment_still_reads_owners";
    if std::env::var_os("GRAPT_ACL_PWSH_CHILD").is_none() {
        let root = Scratch::new()?;
        let local = std::env::var_os("LOCALAPPDATA").map_or_else(|| root.0.clone(), PathBuf::from);
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args(["--exact", WITNESS, "--test-threads=1", "--nocapture"])
            .env("GRAPT_ACL_PWSH_CHILD", "1")
            .env("PSModulePath", PWSH_MODULE_PATH)
            .env("LOCALAPPDATA", local)
            .env("SystemRoot", system_root())
            .kill_on_drop(true)
            .spawn()?;
        let status =
            match tokio::time::timeout(std::time::Duration::from_secs(60), child.wait()).await {
                Ok(status) => status?,
                Err(_) => {
                    child.start_kill()?;
                    tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await??;
                    return Err("the PowerShell 7 environment witness exceeded its deadline".into());
                }
            };
        return if status.success() {
            Ok(())
        } else {
            Err("the PowerShell 7 environment witness failed".into())
        };
    }
    let Some(local) = local_root(WITNESS).await? else {
        return Err("the child has LOCALAPPDATA".into());
    };
    let packages = Scratch::new()?;
    let prefix = local.0.join("installation");
    super::super::commands::run(&update(&fixture(&packages.0, A)?, &prefix)).await?;
    super::super::commands::run(&update(&fixture(&packages.0, B)?, &prefix)).await?;
    assert_eq!(
        transition::state(&prefix)?.ok_or("missing state")?.current,
        B
    );
    Ok(())
}

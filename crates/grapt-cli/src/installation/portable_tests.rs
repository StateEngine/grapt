//! Portable Windows installation never publishes account-level command exposure.
use super::{fixture, system_root, transition, Result, Scratch, A, B};

#[tokio::test]
async fn portable_install_and_update_preserve_private_command_exposure() -> Result {
    let root = Scratch::new()?;
    let Some(local) = std::env::var_os("GRAPT_PORTABLE_INSTALL_TEST") else {
        // A shared temporary directory can grant other accounts write access.
        // Protect only this freshly created fixture before its child installs.
        grapt_login::profile_security::protect_directory(&root.0)?;
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "installation::windows_stage_tests::portable_tests::portable_install_and_update_preserve_private_command_exposure",
                "--test-threads=1",
                "--nocapture",
            ])
            .env_clear()
            .env("GRAPT_PORTABLE_INSTALL_TEST", &root.0)
            .env("LOCALAPPDATA", &root.0)
            .env("TEMP", &root.0)
            .env("TMP", &root.0)
            .env("SystemRoot", system_root())
            .kill_on_drop(true)
            .spawn()?;
        let status =
            match tokio::time::timeout(std::time::Duration::from_secs(20), child.wait()).await {
                Ok(status) => status?,
                Err(_) => {
                    child.start_kill()?;
                    tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await??;
                    return Err("portable installation witness exceeded its deadline".into());
                }
            };
        return if status.success() {
            Ok(())
        } else {
            Err("private portable installation witness failed".into())
        };
    };
    let local = std::path::PathBuf::from(local);
    let prefix = local.join("installation's 工具 space");
    for identity in [A, B] {
        let package = fixture(&local, identity)?;
        let result = crate::installation::run(&[
            "update".into(),
            "--from".into(),
            package.to_string_lossy().into_owned(),
            "--prefix".into(),
            prefix.to_string_lossy().into_owned(),
            "--refresh-launcher".into(),
            "--no-path".into(),
        ])
        .await?;
        assert!(result.contains("USER Path was not changed (--no-path)"));
        let bin =
            crate::operator_text::windows_command_path(&prefix.join("bin")).replace('\'', "''");
        assert!(result.contains(&format!("& '{bin}\\grapt.exe' code")));
        assert!(result.contains(&format!("$env:PATH = '{bin}' + ';' + $env:PATH")));
        assert!(result.contains("this PowerShell session only"));
        assert!(!prefix.join("user-path.json").exists());
        assert!(prefix.join("bin/grapt.exe").is_file());
        assert_eq!(
            transition::state(&prefix)?
                .ok_or("no active build")?
                .current,
            identity
        );
    }
    for arguments in [
        vec!["update", "--prune", "--no-path"],
        vec!["uninstall", "--no-path", "--no-path"],
    ] {
        let arguments = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(super::super::commands::run(&arguments).await.is_err());
    }
    let result = super::super::commands::run(&[
        "uninstall".into(),
        "--prefix".into(),
        prefix.to_string_lossy().into_owned(),
        "--no-path".into(),
    ])
    .await?;
    assert!(result.contains("USER Path was not changed (--no-path)"));
    assert!(prefix.join("versions").is_dir());
    assert!(!prefix.join("user-path.json").exists());
    Ok(())
}

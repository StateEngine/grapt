//! Prepare under writer exclusion; activate under the prefix guard.

#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use grapt_service::installation::PrefixGuard;

use super::{io, package::Package, progress, stage, transition, HELP};

fn default_prefix() -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("installation binary cannot be located: {:?}", error.kind()))?;
    if let Some(prefix) = grapt_service::installation::prefix_of_binary(&executable)
        .map_err(|error| error.to_string())?
    {
        return Ok(prefix);
    }
    #[cfg(windows)]
    if let Some(prefix) = super::windows_bootstrap::installed_prefix(&executable)? {
        return Ok(prefix.to_path_buf());
    }
    user_prefix()
}

#[cfg(windows)]
fn user_prefix() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|local| local.join("grapt"))
        .ok_or_else(|| "no user installation directory is known; name LOCALAPPDATA".into())
}

#[cfg(target_os = "linux")]
fn user_prefix() -> Result<PathBuf, String> {
    let absolute = |key| {
        std::env::var_os(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    absolute("XDG_DATA_HOME")
        .or_else(|| absolute("HOME").map(|home| home.join(".local/share")))
        .map(|directory| directory.join("grapt"))
        .ok_or_else(|| "no user installation directory is known; name --prefix".into())
}

/// One line when an installation from before the rename sits beside this one.
/// It is never touched; upgrades across the rename install a fresh prefix.
fn legacy_prefix_notice(prefix: &Path) -> Option<String> {
    let legacy = super::launchers::legacy_prefix(prefix)?;
    Some(format!(
        "The installation from before the rename at {} was left untouched; point launchers and seat configs at {} when ready.",
        super::display_path(&legacy),
        super::display_path(prefix)
    ))
}

pub(super) fn prefix_directory(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    io::direct_path(path)?;
    // Create only missing ancestors. Inspect each component before any lock or
    // installation effect; a symlink is not treated as a directory.
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        #[cfg(windows)]
        if matches!(part, std::path::Component::Prefix(_)) {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(_) => io::directory(&current)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                #[cfg(unix)]
                let mut builder = std::fs::DirBuilder::new();
                #[cfg(not(unix))]
                let builder = std::fs::DirBuilder::new();
                #[cfg(unix)]
                builder.mode(0o700);
                builder.create(&current).map_err(|error| {
                    format!(
                        "installation directory cannot be created: {:?}",
                        error.kind()
                    )
                })?;
                if let Some(parent) = current.parent() {
                    io::sync_directory(parent)?;
                }
            }
            Err(error) => {
                return Err(format!(
                    "installation directory is unreadable: {:?}",
                    error.kind()
                ))
            }
        }
    }
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) async fn run(arguments: &[String]) -> Result<String, String> {
    if arguments.len() > 7
        || arguments.iter().any(|value| {
            value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control)
        })
    {
        return Err(HELP.into());
    }
    let command = arguments.first().ok_or(HELP)?;
    let mut values = arguments[1..].iter();
    let (mut prefix, mut source, mut profile) = (None, None, None);
    let mut prune = false;
    #[cfg(windows)]
    let mut refresh_launcher = false;
    #[cfg(windows)]
    let mut no_path = false;
    while let Some(option) = values.next() {
        #[cfg(windows)]
        if option == "--no-path" && !no_path {
            no_path = true;
            continue;
        }
        if option == "--prune" && command == "update" && !prune {
            prune = true;
            continue;
        }
        #[cfg(windows)]
        if option == "--refresh-launcher" && command == "update" && !refresh_launcher {
            refresh_launcher = true;
            continue;
        }
        let slot = match option.as_str() {
            "--prefix" if prefix.is_none() => &mut prefix,
            "--from" if command == "update" && source.is_none() => &mut source,
            "--profile" if command == "revert" && profile.is_none() => &mut profile,
            _ => return Err(HELP.into()),
        };
        *slot = Some(PathBuf::from(
            values
                .next()
                .filter(|value| !value.starts_with('-'))
                .ok_or(HELP)?,
        ));
    }
    if command != "update" && command != "revert" && command != "uninstall" {
        return Err(HELP.into());
    }
    if command == "update" && source.is_some() == prune {
        return Err(HELP.into());
    }
    #[cfg(windows)]
    if prune && (refresh_launcher || no_path) {
        return Err(HELP.into());
    }
    let current = std::env::current_dir()
        .map_err(|error| format!("current directory unreadable: {:?}", error.kind()))?;
    let prefix = match prefix {
        Some(prefix) => prefix,
        None => default_prefix()?,
    };
    let prefix = if prefix.is_absolute() {
        prefix
    } else {
        current.join(prefix)
    };
    #[cfg(windows)]
    let prefix = {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or("LOCALAPPDATA is unknown; installation remains unchanged")?;
        super::windows_bootstrap::creation_prefix(&prefix, &local)?
    };
    let source = source.map(|path| {
        if path.is_absolute() {
            path
        } else {
            current.join(path)
        }
    });
    if prune {
        return super::prune::run(&prefix);
    }
    if command == "uninstall" {
        io::directory(&prefix)?;
    }
    prefix_directory(&prefix)?;
    #[cfg(target_os = "linux")]
    let prefix = std::fs::canonicalize(&prefix)
        .map_err(|error| format!("installation prefix cannot be resolved: {:?}", error.kind()))?;
    // Writer -> activation is the only mutation lock order. Preparation can
    // copy/hash inactive versions while normal starts use the active version.
    let _writer = stage::writer(&prefix)?;
    if command == "uninstall" {
        let _guard = PrefixGuard::exclusive(&prefix).map_err(|error| error.to_string())?;
        #[cfg(target_os = "linux")]
        return super::launchers::uninstall(&prefix);
        #[cfg(windows)]
        return if no_path {
            Ok("Installation retained. USER Path was not changed (--no-path).".into())
        } else {
            super::launchers::uninstall(&prefix).await
        };
    }
    let verified_current = progress::during(true, "Verifying current installation", |reporter| {
        transition::verified_current_reporting(&prefix, Some(reporter))
    })?;
    #[cfg(windows)]
    let source_for_notice = source.clone();
    let (target, selected_profile) = if let Some(source) = source {
        let package = Package::verify_reporting(&source, true, progress::VERIFY_PACKAGE)?;
        (
            Some(stage::install_reporting(&prefix, &package, true)?),
            None,
        )
    } else {
        let previous = transition::revert_predecessor(&prefix)?;
        let target = previous
            .map(|previous| {
                Package::verify_reporting(
                    &prefix.join("versions").join(previous).join("bin"),
                    true,
                    "Verifying previous build",
                )
            })
            .transpose()?;
        let arguments = profile
            .map(|profile| {
                vec![
                    "--profile".to_owned(),
                    profile.to_string_lossy().into_owned(),
                ]
            })
            .unwrap_or_default();
        let selected = crate::command::terminal::TerminalCommand::parse(
            &arguments,
            current,
            crate::command::terminal::user_profile(|name| std::env::var_os(name)),
        )?;
        (target, Some(selected.profile))
    };
    // Compatibility aliases join the target version before its permission
    // check and activation. Each is a copy re-hashed against its signed target.
    if let Some(target) = &target {
        super::inventory::materialize(target)?;
    }
    // Before the launcher is published or anything is activated: another
    // account that can write the prefix could swap what the launcher runs.
    #[cfg(windows)]
    if let Some(target) = &target {
        super::windows_acl::verify(&prefix, &target.commit)?;
    }
    #[cfg(windows)]
    let bootstrap = target
        .as_ref()
        .map(|target| super::windows_bootstrap::prepare_update(&prefix, target, refresh_launcher))
        .transpose()?;
    #[cfg(windows)]
    let se_bootstrap = target
        .as_ref()
        .filter(|target| target.stateengine_entry.is_some())
        .map(|target| {
            super::windows_bootstrap::prepare_update_named(
                &prefix,
                target,
                refresh_launcher,
                "se.exe",
                "se-bootstrap.json",
            )
        })
        .transpose()?;
    // Size enumeration belongs to preparation, too; no content hashing or
    // broad tree walk is added while starts wait on the activation guard.
    let inventory_hint = if command == "update" {
        Some(super::prune::hint_inventory(&prefix))
    } else {
        None
    };
    let _guard = PrefixGuard::exclusive(&prefix).map_err(|error| error.to_string())?;
    transition::recover_verified(&prefix, verified_current.as_ref())?;
    let target = target.ok_or_else(|| {
        if verified_current.is_some() {
            "installation has no previous build to revert to"
        } else {
            "installation has no current build"
        }
    })?;
    if let Some(profile) = selected_profile {
        let state = transition::state(&prefix)?.ok_or("installation has no current build")?;
        if state.previous.as_deref() != Some(target.commit.as_str()) {
            return Err("revert predecessor changed during verification; retry".into());
        }
        compatible(&prefix, &target.commit, target.inventory.server(), &profile).await?;
    }
    #[cfg(windows)]
    let launcher_differs = bootstrap
        .as_ref()
        .ok_or("installation has no verified launcher")?
        .finish()?;
    #[cfg(windows)]
    if let Some(bootstrap) = se_bootstrap {
        bootstrap.finish()?;
    }
    #[cfg(target_os = "linux")]
    if target.stateengine_entry.is_some() {
        super::launchers::prepare_bundled(&prefix, &target)?;
    }
    #[cfg(target_os = "linux")]
    let path_notice = super::launchers::install(&prefix, target.stateengine_entry.is_some())?;
    #[cfg(windows)]
    let path_notice = if no_path {
        "USER Path was not changed (--no-path).".to_owned()
    } else {
        super::launchers::install(&prefix, target.stateengine_entry.is_some()).await?
    };
    transition::activate_verified(&prefix, &target)?;
    // Command links follow the active version's own CLI name, so `grapt`
    // keeps working after a revert to a pre-rename build. Refreshed on every
    // activation; the build is already active if this fails.
    #[cfg(target_os = "linux")]
    let cli = target.inventory.cli();
    #[cfg(target_os = "linux")]
    let refreshed = super::launchers::refresh(&prefix, cli);
    #[cfg(windows)]
    let launcher_source = source_for_notice.unwrap_or_else(|| target.root.clone());
    let target = target.commit;
    #[cfg(target_os = "linux")]
    let (launcher, bin) = (Path::new("current/bin").join(cli), "current/bin");
    #[cfg(target_os = "linux")]
    let message = format!(
        "active build {target}\nStart {} code (or add {} to PATH).",
        super::display_path(&prefix.join(launcher)),
        super::display_path(&prefix.join(bin))
    );
    #[cfg(windows)]
    let message = format!(
        "active build {target}\n{}",
        crate::operator_text::windows_start_notice(&prefix),
    );
    // The build is active now; history only words the notice, so an
    // unreadable history leaves the direction unknown rather than failing.
    #[cfg(windows)]
    let notice = launcher_differs.and_then(|launcher| {
        let activations = transition::activations(&prefix).unwrap_or_default();
        let age = crate::operator_text::launcher_age(&activations, &launcher, &target);
        super::windows_bootstrap::refresh_notice(&prefix, &launcher_source, age, no_path)
    });
    #[cfg(windows)]
    let message = match notice {
        Some(notice) => format!("{message}\n{notice}"),
        None => message,
    };
    let mut message = format!("{message}\n{path_notice}");
    message.extend(legacy_prefix_notice(&prefix).map(|notice| format!("\n{notice}")));
    #[cfg(target_os = "linux")]
    if let Err(reason) = refreshed {
        message.push_str(&format!(
            "\nCommand links were not refreshed to this build's {cli}: {reason}"
        ));
    }
    if let Some(inventory) = inventory_hint {
        match inventory.and_then(|inventory| super::prune::hint(&prefix, &inventory)) {
            Ok(Some(hint)) => {
                message.push('\n');
                message.push_str(&hint);
            }
            Ok(None) => {}
            Err(reason) => message.push_str(&format!(
                "\nUpdate completed; old-version inspection is unavailable: {reason}"
            )),
        }
    }
    Ok(message)
}

/// Run that version's own server, named by its recorded inventory, so a
/// retained pre-rename build checks the profile with its own server binary.
async fn compatible(
    prefix: &Path,
    version: &str,
    server: &str,
    profile: &Path,
) -> Result<(), String> {
    let binary = prefix
        .join("versions")
        .join(version)
        .join("bin")
        .join(server);
    let mut child =
        tokio::process::Command::from(grapt_service::launcher_environment::command(binary))
            .arg("check-profile")
            .arg("--service")
            .arg(profile)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                format!(
                    "older build's read-only profile check cannot start: {:?}; revert refused",
                    error.kind()
                )
            })?;
    let result = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    match result {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(_)) => Err(
            "older build refused its read-only profile check; installation remains unchanged"
                .into(),
        ),
        Ok(Err(error)) => Err(format!(
            "older build check failed: {:?}; installation remains unchanged",
            error.kind()
        )),
        Err(_) => {
            child.start_kill().map_err(|error| {
                format!("older build check could not be stopped: {:?}", error.kind())
            })?;
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .map_err(|_| "older build check could not be reaped within 5 seconds")?
                .map_err(|error| format!("older build check reap failed: {:?}", error.kind()))?;
            Err("older build check exceeded 10 seconds; revert refused".into())
        }
    }
}

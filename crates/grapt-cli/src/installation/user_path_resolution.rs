//! Read-only System Path resolution, run inside the bounded registry worker.
use std::path::Path;

fn expanded(
    value: &str,
    resolve: &mut impl FnMut(&str) -> Result<Option<String>, String>,
) -> Result<String, String> {
    let mut output = String::new();
    let mut rest = value;
    for _ in 0..64 {
        let Some((literal, tail)) = rest.split_once('%') else {
            output.push_str(rest);
            return if output.len() <= super::CAP {
                Ok(output)
            } else {
                Err("expanded System Path directory exceeds its bound".into())
            };
        };
        output.push_str(literal);
        let Some((name, tail)) = tail.split_once('%') else {
            return Err("System Path contains an unresolved percent reference; repair it or use a fresh terminal before installation".into());
        };
        output.push_str(&resolve(name)?.ok_or("System Path contains an unknown environment variable; use a fresh terminal or repair it before installation")?);
        if output.len() > super::CAP {
            return Err("expanded System Path directory exceeds its bound".into());
        }
        rest = tail;
    }
    Err("System Path expansion exceeds its reference bound".into())
}

fn process_extensions() -> Result<Option<String>, String> {
    // Extensions are applied by the user's shell after environment merging.
    std::env::var_os("PATHEXT")
        .map(|value| {
            value
                .into_string()
                .map_err(|_| "PATHEXT is not valid Unicode".into())
        })
        .transpose()
}

pub(super) fn verify(system: &str, expand: bool, entry: &str, bundled: bool) -> Result<(), String> {
    #[cfg(not(test))]
    let mut resolve = super::system_value;
    #[cfg(test)]
    let mut resolve = |_: &str| Ok(None);
    verify_with(
        system,
        expand,
        entry,
        bundled,
        &mut resolve,
        process_extensions()?,
    )
}

fn verify_with(
    system: &str,
    expand: bool,
    entry: &str,
    bundled: bool,
    resolve: &mut impl FnMut(&str) -> Result<Option<String>, String>,
    pathext: Option<String>,
) -> Result<(), String> {
    let system = if expand {
        expanded(system, resolve)?
    } else {
        system.into()
    };
    let mut extensions = vec![
        "exe".to_owned(),
        "com".into(),
        "cmd".into(),
        "bat".into(),
        "ps1".into(),
    ];
    if let Some(value) = pathext {
        if value.len() > 1024 {
            return Err("PATHEXT exceeds its command-resolution bound".into());
        }
        for (index, value) in value.split(';').enumerate() {
            if index >= 32 {
                return Err("PATHEXT exceeds 32 extensions".into());
            }
            if value.is_empty() {
                continue;
            }
            let value = value
                .strip_prefix('.')
                .filter(|value| {
                    !value.is_empty()
                        && value.len() <= 16
                        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
                .ok_or("PATHEXT contains an unsupported command extension")?
                .to_ascii_lowercase();
            if !extensions.contains(&value) {
                extensions.push(value);
            }
        }
    }
    let commands: &[&str] = if bundled {
        &["grapt", "se"]
    } else {
        &["grapt"]
    };
    for (index, directory) in system.split(';').enumerate() {
        if index >= 512 {
            return Err("System Path exceeds 512 directories; command resolution refused".into());
        }
        if directory.trim().is_empty() {
            continue;
        }
        let directory = directory.trim().trim_matches('"');
        if !Path::new(directory).is_absolute() {
            return Err("System Path contains a relative directory that can shadow installed commands; repair it before installation".into());
        }
        if super::normalize(directory) == super::normalize(entry) {
            return Ok(());
        }
        for name in commands {
            for extension in &extensions {
                match std::fs::symlink_metadata(Path::new(directory).join(format!("{name}.{extension}"))) {
                    Ok(_) => return Err(format!("System Path shadows {name} before this installation's USER Path entry; remove or reorder that conflicting System Path command before retrying. System Path was not changed")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => return Err(format!("System Path command resolution is unreadable: {:?}; installation refused", error.kind())),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn native_machine_shadow_refuses_a_first_user_entry() -> Result<(), String> {
        let root = Scratch(std::env::temp_dir().join(format!(
            "grapt-system-path-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        let system = root.0.join("system");
        let owned = root.0.join("owned");
        for directory in [&system, &owned] {
            std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        }
        let entry = owned.to_str().ok_or("fixture path is not Unicode")?;
        let current = super::super::Snapshot {
            kind: Some(false),
            bytes: Vec::new(),
        }
        .with_text(entry)?;
        assert!(super::super::plan_install(&current, entry, None)?.is_none());
        let machine = system.to_str().ok_or("fixture path is not Unicode")?;
        for command in ["se.exe", "grapt.exe", "se.cmd"] {
            let conflict = system.join(command);
            std::fs::write(&conflict, b"synthetic command; never execute")
                .map_err(|error| error.to_string())?;
            assert!(
                verify(machine, false, entry, true)
                    .is_err_and(|reason| reason.contains("System Path shadows")),
                "a first USER entry was accepted behind a shadowing System command"
            );
            assert!(verify_with(
                "%DIRECTORIES%",
                true,
                entry,
                true,
                &mut |name| { Ok((name == "DIRECTORIES").then(|| format!("{machine};{entry}"))) },
                None
            )
            .is_err_and(|reason| reason.contains("System Path shadows")));
            let machine_tools = machine.to_owned();
            let user_tools = entry.to_owned();
            let mut system_environment = |name: &str| {
                Ok(name
                    .eq_ignore_ascii_case("TOOLS")
                    .then(|| machine_tools.clone()))
            };
            let mut merged_environment = |name: &str| {
                Ok(name
                    .eq_ignore_ascii_case("TOOLS")
                    .then(|| user_tools.clone()))
            };
            assert!(
                verify_with("%TOOLS%", true, entry, true, &mut system_environment, None)
                    .is_err_and(|reason| reason.contains("System Path shadows")),
                "USER TOOLS override hid the System command"
            );
            assert!(
                verify_with("%TOOLS%", true, entry, true, &mut merged_environment, None).is_ok()
            );
            assert!(verify(&format!("{entry};{machine}"), false, entry, true).is_ok());
            std::fs::remove_file(conflict).map_err(|error| error.to_string())?;
        }
        let custom = system.join("se.fixture");
        std::fs::write(&custom, b"synthetic association; never execute")
            .map_err(|error| error.to_string())?;
        assert!(verify_with(
            machine,
            false,
            entry,
            true,
            &mut |_| Ok(None),
            Some(".FIXTURE".into())
        )
        .is_err_and(|reason| reason.contains("System Path shadows")));
        std::fs::remove_file(custom).map_err(|error| error.to_string())?;
        verify(machine, false, entry, true)?;
        Ok(())
    }
    #[test]
    fn system_expansion_is_bounded_and_unknown_or_relative_paths_refuse() -> Result<(), String> {
        assert_eq!(
            expanded("%ROOT%/bin", &mut |name| Ok(
                (name == "ROOT").then(|| "/system".into())
            ))?,
            "/system/bin"
        );
        assert!(expanded("%UNKNOWN%/bin", &mut |_| Ok(None)).is_err());
        assert!(expanded(&"%ROOT%".repeat(65), &mut |_| Ok(Some(String::new()))).is_err());
        assert!(verify("relative-directory", false, "/owned", true).is_err());
        assert!(verify(&";".repeat(512), false, "/owned", true)
            .is_err_and(|reason| reason.contains("512 directories")));
        assert!(verify_with(
            "",
            false,
            "/owned",
            true,
            &mut |_| Ok(None),
            Some("not-an-extension".into())
        )
        .is_err_and(|reason| reason.contains("PATHEXT")));
        Ok(())
    }
}

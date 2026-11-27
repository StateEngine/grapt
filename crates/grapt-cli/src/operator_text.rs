//! Human-facing paths and installer guidance; these strings never select I/O paths.

use std::path::Path;

pub(crate) fn opening_repository(directory: &str, windows: bool) -> String {
    let display = if windows {
        let normalized = directory.replace('/', "\\");
        if let Some(unc) = normalized.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            normalized
                .strip_prefix(r"\\?\")
                .unwrap_or(&normalized)
                .to_owned()
        }
    } else {
        directory.to_owned()
    };
    format!("Opening repository {display}")
}

pub(crate) fn windows_command_path(path: &Path) -> String {
    let display = path.to_string_lossy().replace('/', "\\");
    let Some(ordinary) = display.strip_prefix(r"\\?\") else {
        return display;
    };
    let bytes = ordinary.as_bytes();
    let plain_drive =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    let ordinary_names = ordinary
        .get(3..)
        .unwrap_or_default()
        .split('\\')
        .all(|name| {
            let stem = name
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || stem
                    .strip_prefix("COM")
                    .or_else(|| stem.strip_prefix("LPT"))
                    .is_some_and(|suffix| {
                        suffix.len() == 1 && b"123456789".contains(&suffix.as_bytes()[0])
                    });
            !device && !name.ends_with('.') && !name.ends_with(' ')
        });
    if plain_drive && ordinary.encode_utf16().count() < 260 && ordinary_names {
        ordinary.to_owned()
    } else {
        display
    }
}

#[cfg(any(windows, test))]
fn powershell_path_literal(path: &Path) -> String {
    let mut literal = String::from("'");
    for character in windows_command_path(path).chars() {
        literal.push(character);
        // PowerShell also treats typographic apostrophes as string delimiters.
        if matches!(
            character,
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}'
        ) {
            literal.push(character);
        }
    }
    literal.push('\'');
    literal
}

/// Copyable commands for the installed launcher, without changing persistent PATH.
#[cfg(any(windows, test))]
pub(crate) fn windows_start_notice(prefix: &Path) -> String {
    let bin = prefix.join("bin");
    format!(
        "Start in PowerShell:\n& {} code\nTo use commands in this PowerShell session only:\n$env:PATH = {} + ';' + $env:PATH",
        powershell_path_literal(&bin.join("grapt.exe")),
        powershell_path_literal(&bin),
    )
}

/// How the fixed launcher's build relates to the build being activated.
/// Builds carry no order of their own, so the order is this installation's:
/// when each was FIRST activated.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LauncherAge {
    Same,
    Older,
    Newer,
    Unknown,
}

#[cfg(any(windows, test))]
pub(crate) fn launcher_age(activations: &[String], launcher: &str, target: &str) -> LauncherAge {
    if launcher == target {
        return LauncherAge::Same;
    }
    let first = |build: &str| activations.iter().position(|active| active == build);
    match (first(launcher), first(target)) {
        (Some(launcher), Some(target)) if launcher < target => LauncherAge::Older,
        (Some(_), Some(_)) => LauncherAge::Newer,
        _ => LauncherAge::Unknown,
    }
}

/// What to tell a user whose fixed launcher differs from the build just
/// activated. A launcher newer than a build the user deliberately went back
/// to needs no refresh, and the notice never offers one toward that build.
#[cfg(any(windows, test))]
pub(crate) fn launcher_refresh_notice(
    prefix: &Path,
    source: &Path,
    age: LauncherAge,
    no_path: bool,
) -> Option<String> {
    let refresh = format!(
        "Refreshing is optional. The command below installs this package's launcher and selects this build.\nQuit installed Grapt apps, then run in PowerShell:\n& {} update --from {} --prefix {} --refresh-launcher{}",
        powershell_path_literal(&source.join("grapt.exe")), powershell_path_literal(source), powershell_path_literal(prefix),
        if no_path { " --no-path" } else { "" }
    );
    match age {
        LauncherAge::Same => None,
        LauncherAge::Newer => Some("The fixed launcher comes from a build first installed after this one; it still selects the active build.\nNo refresh is needed after a deliberate return to an earlier build.".to_owned()),
        LauncherAge::Older => Some(format!("The fixed launcher comes from a build first installed before this one; it still selects the active build.\n{refresh}")),
        LauncherAge::Unknown => Some(format!("The fixed launcher differs from the active build; it still selects the active build.\n{refresh}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_start_commands_quote_paths_and_preserve_session_path() {
        for (prefix, bin) in [
            (r"C:\tools\grapt", r"C:\tools\grapt\bin"),
            (r"C:/my tools/grapt", r"C:\my tools\grapt\bin"),
            (r"C:\O'Brien\工具", r"C:\O''Brien\工具\bin"),
            (r"C:\O‘Brien\工具", r"C:\O‘‘Brien\工具\bin"),
            (r"C:\O’Brien\工具", r"C:\O’’Brien\工具\bin"),
            (r"C:\O‚Brien\工具", r"C:\O‚‚Brien\工具\bin"),
            (r"C:\O‛Brien\工具", r"C:\O‛‛Brien\工具\bin"),
            (r"C:\$cash`tick & tools", r"C:\$cash`tick & tools\bin"),
            (r"\\?\C:\my tools\工具", r"C:\my tools\工具\bin"),
            (r"\\server\share\tools", r"\\server\share\tools\bin"),
            (
                r"\\?\UNC\server\share\tools",
                r"\\?\UNC\server\share\tools\bin",
            ),
            (r"\\?\C:\name.", r"\\?\C:\name.\bin"),
        ] {
            assert_eq!(
                windows_start_notice(Path::new(prefix)),
                format!(
                    "Start in PowerShell:\n& '{bin}\\grapt.exe' code\nTo use commands in this PowerShell session only:\n$env:PATH = '{bin}' + ';' + $env:PATH"
                ),
                "{prefix}",
            );
        }
    }

    #[test]
    fn windows_start_commands_keep_required_verbatim_spelling() {
        let prefix = format!(r"\\?\C:\{}", "x".repeat(260));
        let notice = windows_start_notice(Path::new(&prefix));
        assert!(notice.contains(&format!("& '{prefix}\\bin\\grapt.exe' code")));
        assert!(notice.ends_with(&format!("$env:PATH = '{prefix}\\bin' + ';' + $env:PATH")));
    }

    #[test]
    fn opening_repository_shows_a_user_path_without_changing_the_io_path() {
        let io_path = r"\\?\C:\Users\example\my repo";
        assert_eq!(
            opening_repository(io_path, true),
            r"Opening repository C:\Users\example\my repo"
        );
        assert_eq!(io_path, r"\\?\C:\Users\example\my repo");
        assert_eq!(
            opening_repository(r"\\?\UNC\server\share\repo", true),
            r"Opening repository \\server\share\repo"
        );
        assert_eq!(
            opening_repository("/home/example/repo", false),
            "Opening repository /home/example/repo"
        );
    }

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const COMMAND: &str = "Refreshing is optional. The command below installs this package's launcher and selects this build.\nQuit installed Grapt apps, then run in PowerShell:\n& 'C:\\packages\\b\\grapt.exe' update --from 'C:\\packages\\b' --prefix 'C:\\Users\\example\\grapt' --refresh-launcher";

    fn history(builds: &[&str]) -> Vec<String> {
        builds.iter().map(|build| (*build).to_owned()).collect()
    }

    fn notice(age: LauncherAge) -> Option<String> {
        launcher_refresh_notice(
            Path::new(r"C:\Users\example\grapt"),
            Path::new(r"C:\packages\b"),
            age,
            false,
        )
    }

    #[test]
    fn portable_refresh_commands_preserve_the_explicit_no_path_choice() {
        for age in [LauncherAge::Older, LauncherAge::Unknown] {
            let Some(portable) = launcher_refresh_notice(
                Path::new(r"C:\private\grapt"),
                Path::new(r"C:\next package"),
                age,
                true,
            ) else {
                panic!("a differing launcher needs a refresh notice");
            };
            assert!(portable.ends_with("--refresh-launcher --no-path"));
            assert!(!notice(age).is_some_and(|ordinary| ordinary.contains("--no-path")));
        }
    }

    /// J07: install A, update to B, revert to A, update to B again.
    /// Each row is (activation history after the step, launcher, target).
    #[test]
    fn j07_without_a_refresh_offers_one_only_toward_the_later_build() {
        let steps = [
            (history(&[A]), A, A, LauncherAge::Same),
            (history(&[A, B]), A, B, LauncherAge::Older),
            (history(&[A, B, A]), A, A, LauncherAge::Same),
            (history(&[A, B, A, B]), A, B, LauncherAge::Older),
        ];
        for (activations, launcher, target, age) in steps {
            assert_eq!(launcher_age(&activations, launcher, target), age);
        }
    }

    /// The same sequence after the user refreshed to B's launcher at step 2:
    /// the revert to A is a deliberate return, and nothing points back at A.
    #[test]
    fn j07_after_a_refresh_never_offers_one_toward_the_earlier_build() {
        let steps = [
            (history(&[A, B, A]), B, A, LauncherAge::Newer),
            (history(&[A, B, A, B]), B, B, LauncherAge::Same),
        ];
        for (activations, launcher, target, age) in steps {
            assert_eq!(launcher_age(&activations, launcher, target), age);
        }
    }

    #[test]
    fn pruned_or_archived_history_leaves_the_direction_unknown() {
        assert_eq!(launcher_age(&[], B, A), LauncherAge::Unknown);
        assert_eq!(launcher_age(&history(&[B]), B, A), LauncherAge::Unknown);
        assert_eq!(launcher_age(&history(&[A]), B, A), LauncherAge::Unknown);
    }

    #[test]
    fn the_notice_names_the_direction_and_offers_a_refresh_only_forward() {
        assert_eq!(notice(LauncherAge::Same), None);
        assert_eq!(
            notice(LauncherAge::Newer).as_deref(),
            Some("The fixed launcher comes from a build first installed after this one; it still selects the active build.\nNo refresh is needed after a deliberate return to an earlier build.")
        );
        assert_eq!(
            notice(LauncherAge::Older),
            Some(format!("The fixed launcher comes from a build first installed before this one; it still selects the active build.\n{COMMAND}"))
        );
        assert_eq!(
            notice(LauncherAge::Unknown),
            Some(format!("The fixed launcher differs from the active build; it still selects the active build.\n{COMMAND}"))
        );
    }

    #[test]
    fn suggested_windows_commands_use_backslashes_and_powershell_quoting() {
        let Some(notice) = launcher_refresh_notice(
            Path::new(r"C:/Users/example/grapt"),
            Path::new(r"E:/temp/package's"),
            LauncherAge::Older,
            false,
        ) else {
            panic!("an older launcher is told it may refresh");
        };
        assert!(notice.ends_with("& 'E:\\temp\\package''s\\grapt.exe' update --from 'E:\\temp\\package''s' --prefix 'C:\\Users\\example\\grapt' --refresh-launcher"));
        for path in [r"\\?\C:\name.", r"\\?\C:\CON", r"\\?\UNC\server\share"] {
            assert_eq!(windows_command_path(Path::new(path)), path);
        }
        let long = format!(r"\\?\C:\{}", "x".repeat(257));
        assert_eq!(windows_command_path(Path::new(&long)), long);
    }
}

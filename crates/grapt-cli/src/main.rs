use grapt_cli::{command_error_exit, execute, CliCommand};

#[tokio::main]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn main() {
    let arguments: Vec<String> = match std::env::args_os()
        .skip(1)
        .map(std::ffi::OsString::into_string)
        .collect::<Result<_, _>>()
    {
        Ok(arguments) => arguments,
        Err(_) => std::process::exit(i32::from(command_error_exit(
            &"command arguments must be valid Unicode",
        ))),
    };
    // Ephemeral policy is chosen before even installation preparation: that
    // normal path can write profiles, logs and registration/installation state.
    if let Some(command) = grapt_cli::command::ephemeral::parse_early(&arguments) {
        let code = match command {
            Ok(command) => execute(command).await,
            Err(error) => command_error_exit(&error),
        };
        std::process::exit(i32::from(code));
    }
    #[cfg(any(target_os = "linux", windows))]
    let bundled = std::env::args_os().next().is_some_and(|value| {
        std::path::Path::new(&value)
            .file_name()
            .is_some_and(|name| {
                name == "se"
                    || cfg!(windows) && name.to_string_lossy().eq_ignore_ascii_case("se.exe")
            })
    });
    #[cfg(any(target_os = "linux", windows))]
    if bundled {
        let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        match grapt_cli::installation::bundled_stateengine(&raw).await {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                eprintln!("se: {error}");
                std::process::exit(2);
            }
        }
    }
    // Standalone command help precedes installation and profile discovery.
    let help = std::env::args_os()
        .skip(1)
        .map(std::ffi::OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .and_then(|arguments| {
            let (command, rest) = arguments.split_first()?;
            grapt_cli::command::help::parse(command, rest)
        });
    if let Some(topic) = help {
        std::process::exit(i32::from(execute(CliCommand::FocusedHelp(topic)).await));
    }
    #[cfg(windows)]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|value| value == "--installation-user-path")
    {
        std::process::exit(grapt_cli::installation::user_path_worker());
    }
    #[cfg(windows)]
    {
        let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        match grapt_cli::installation::windows_redirect(&raw).await {
            Ok(Some(code)) => std::process::exit(code),
            Ok(None) => {}
            Err(error) => {
                eprintln!("grapt: {error}");
                std::process::exit(2);
            }
        }
    }
    if arguments.first().is_some_and(|value| {
        matches!(
            value.as_str(),
            "update" | "revert" | "uninstall" | "recover-sign-in"
        )
    }) {
        let code = match grapt_cli::installation::run(&arguments).await {
            Ok(message) => {
                println!("{message}");
                0
            }
            Err(error) => {
                eprintln!("grapt: {error}");
                2
            }
        };
        std::process::exit(code);
    }
    #[cfg(any(target_os = "linux", windows))]
    let _prefix_guard = {
        let start = match grapt_cli::installation::prepare() {
            Ok(start) => start,
            Err(error) => {
                eprintln!("grapt: {error}");
                std::process::exit(2);
            }
        };
        if let Some(binary) = start.redirect {
            match tokio::process::Command::from(grapt_service::launcher_environment::command(
                binary,
            ))
            .args(std::env::args_os().skip(1))
            .status()
            .await
            {
                Ok(status) => std::process::exit(status.code().unwrap_or(2)),
                Err(error) => {
                    eprintln!("grapt: active build cannot start: {:?}", error.kind());
                    std::process::exit(2);
                }
            }
        }
        start.guard
    };
    let code = if arguments.is_empty() {
        terminal(&[]).await
    } else if arguments.first().is_some_and(|value| value == "code") {
        terminal(&arguments[1..]).await
    } else {
        match CliCommand::parse(arguments) {
            Ok(command) => execute(command).await,
            Err(error) => command_error_exit(&error),
        }
    };
    // Exit without joining runtime threads: a run cancelled while its stdout
    // reader was not draining leaves one blocked in that write.
    std::process::exit(i32::from(code))
}

async fn terminal(arguments: &[String]) -> u8 {
    use grapt_cli::command::terminal::{TerminalCommand, HELP};
    if arguments == ["--help"] || arguments == ["-h"] {
        println!("{HELP}");
        return 0;
    }
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => {
            eprintln!(
                "grapt: current repository is unreadable: {:?}",
                error.kind()
            );
            return 2;
        }
    };
    let user_profile = grapt_cli::command::terminal::user_profile(|name| std::env::var_os(name));
    let result = match TerminalCommand::parse(arguments, current, user_profile) {
        Ok(command) => grapt_cli::client::terminal::run(command).await,
        Err(error) => Err(error.into()),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("grapt: {error}");
            error.exit_class().code()
        }
    }
}

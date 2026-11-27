//! The server's argument grammar: each verb and the exact argument shape it admits.
use std::path::PathBuf;

pub(crate) enum Command {
    InitEmptyProfile {
        service_root: PathBuf,
    },
    Usage {
        service_root: PathBuf,
        session: String,
        prices: Option<PathBuf>,
    },
    InitProfile {
        service_root: PathBuf,
        source: PathBuf,
    },
    CodeModeWorker,
    Version,
    Startup {
        service_root: PathBuf,
        slot: String,
        environment: String,
        instance: String,
    },
    Serve {
        service_root: PathBuf,
    },
    ServeService {
        service_root: PathBuf,
        external_host: bool,
    },
    ServeConnections {
        service_root: PathBuf,
    },
    ServeTerminal {
        service_root: PathBuf,
        notify_ready: bool,
        approval: Option<String>,
    },
    /// Serve after an explicitly invoked, validated service handover.
    ///
    /// A separate verb rather than a flag on `serve`, so that ordinary startup
    /// cannot acquire handover behaviour by accident and nothing a running
    /// installation writes to disk can trigger it.
    Handover {
        service_root: PathBuf,
        run_root: PathBuf,
        run_id: String,
        engine_version: String,
    },
    /// Activate a pinned tool binary through the verified boundary.
    ///
    /// A separate verb: ordinary startup never acquires activation
    /// behaviour, and the boundary decides everything this verb asks of it.
    /// The target must describe this host (architecture and operating
    /// system): the seam activates the host's own tool tree.
    ToolchainActivate {
        root: PathBuf,
        target: grapt_toolchain::TargetTriple,
        pin: PathBuf,
    },
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn parse(args: &[String]) -> Result<Command, String> {
    if (args.len() == 5 || args.len() == 7)
        && args[0] == "usage"
        && args[1] == "--service"
        && args[3] == "--session"
    {
        let prices = if args.len() == 7 {
            if args[5] != "--prices" {
                return Err("usage expects --prices <policy.json>".into());
            }
            Some(PathBuf::from(&args[6]))
        } else {
            None
        };
        return Ok(Command::Usage {
            service_root: PathBuf::from(&args[2]),
            session: args[4].clone(),
            prices,
        });
    }
    if args.len() == 5
        && args[0] == "serve-connections"
        && args[1] == "--service"
        && args[3] == "--port"
        && args[4] == "0"
    {
        return Ok(Command::ServeConnections {
            service_root: PathBuf::from(&args[2]),
        });
    }
    if (args.len() == 5 || (args.len() == 6 && args[5] == "--external-host"))
        && args[0] == "serve-service"
        && args[1] == "--service"
        && args[3] == "--port"
        && args[4] == "0"
    {
        return Ok(Command::ServeService {
            service_root: PathBuf::from(&args[2]),
            external_host: args.len() == 6,
        });
    }
    if (args.len() == 5 || (args.len() == 6 && args[5] == "--notify-ready"))
        && args[0] == "serve-terminal"
        && args[1] == "--service"
        && args[3] == "--port"
        && args[4] == "0"
    {
        return Ok(Command::ServeTerminal {
            service_root: PathBuf::from(&args[2]),
            notify_ready: args.len() == 6,
            approval: None,
        });
    }
    if (args.len() == 7 || (args.len() == 8 && args[7] == "--notify-ready"))
        && args[0] == "serve-terminal"
        && args[1] == "--profile-approval"
        && args[3] == "--service"
        && args[5] == "--port"
        && args[6] == "0"
    {
        return Ok(Command::ServeTerminal {
            service_root: PathBuf::from(&args[4]),
            approval: Some(args[2].clone()),
            notify_ready: args.len() == 8,
        });
    }
    if args.len() == 3 && args[0] == "init-profile" && args[1] == "--service" {
        return Ok(Command::InitEmptyProfile {
            service_root: PathBuf::from(&args[2]),
        });
    }
    if args.len() == 5 && args[0] == "init-profile" && args[1] == "--service" && args[3] == "--from"
    {
        return Ok(Command::InitProfile {
            service_root: PathBuf::from(&args[2]),
            source: PathBuf::from(&args[4]),
        });
    }
    if args.first().is_some_and(|arg| arg == "init-local") {
        return Err(grapt_platform::ConfigPublicationError::LocalModeRemoved.to_string());
    }

    if args == [grapt_codemode::WORKER_ARGUMENT] {
        return Ok(Command::CodeModeWorker);
    }
    if args == ["--version"] {
        return Ok(Command::Version);
    }
    if args.len() == 9
        && args[0] == "startup"
        && args[1] == "--service"
        && args[3] == "--slot"
        && args[5] == "--environment"
        && args[7] == "--instance"
    {
        return Ok(Command::Startup {
            service_root: PathBuf::from(&args[2]),
            slot: args[4].clone(),
            environment: args[6].clone(),
            instance: args[8].clone(),
        });
    }
    if args.len() == 9
        && args[0] == "handover"
        && args[1] == "--service"
        && args[3] == "--run-root"
        && args[5] == "--run-id"
        && args[7] == "--engine-version"
    {
        return Ok(Command::Handover {
            service_root: PathBuf::from(&args[2]),
            run_root: PathBuf::from(&args[4]),
            run_id: args[6].clone(),
            engine_version: args[8].clone(),
        });
    }
    if args.len() == 8
        && args[0] == "toolchain"
        && args[1] == "activate"
        && args[2] == "--root"
        && args[4] == "--target"
        && args[6] == "--pin"
    {
        return match grapt_toolchain::TargetTriple::new(&args[5]) {
            Some(target) => {
                // The seam activates the host's own tool tree: the triple
                // must name this host's architecture and operating system.
                let host_os = match std::env::consts::OS {
                    "macos" => "darwin",
                    other => other,
                };
                let parts: Vec<&str> = target.as_str().split('-').collect();
                if parts.len() < 3 || parts[0] != std::env::consts::ARCH || parts[2] != host_os {
                    return Err(
                        "--target must describe this host: it names a different architecture or operating system".to_owned(),
                    );
                }
                Ok(Command::ToolchainActivate {
                    root: PathBuf::from(&args[3]),
                    target,
                    pin: PathBuf::from(&args[7]),
                })
            }
            None => Err("--target must be a release triple".to_owned()),
        };
    }
    if args.len() != 5 || args[0] != "serve" || args[1] != "--service" || args[3] != "--port" {
        return Err("usage: grapt-server.exe --version \
             | --external-host-capabilities \
             | usage --service <root> --session <ses_id> [--prices <policy.json>] \
             | serve --service <root> --port 0 \
             | serve-service --service <root> --port 0 [--external-host] \
             | handover --service <root> --run-root <dir> --run-id <id> --engine-version <commit> \
             | toolchain activate --root <root> --target <triple> --pin <pin>"
            .to_owned());
    }
    if args[4] != "0" {
        return Err("IA0-1R admits only --port 0".to_owned());
    }
    Ok(Command::Serve {
        service_root: PathBuf::from(&args[2]),
    })
}

//! The server executable's entry point: verb dispatch over the product composition.
use std::io;
use std::path::PathBuf;

use crate::command::{parse, Command};
use crate::receipt::{refuse, refuse_as, LaunchReceipt};
use crate::stateengine::StateEngine;
use crate::{cleanup_command, compose, dogfood, handover, installation, retire_service, startup};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Runs the server's command line and exits the process with the command's status.
///
/// The open build passes `None`; only a private composition root links an engine.
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn run(stateengine: Option<StateEngine>) {
    if stateengine.is_some() {
        std::process::exit(refuse(
            "this build cannot host a supplied StateEngine yet; launch it without one",
        ));
    }
    // A terminal's pseudoconsole root runs before anything else: it owns no
    // product state and must exit with its shell's status unchanged.
    #[cfg(windows)]
    {
        let shim: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        if let Some(code) = dogfood::terminal_shim_main(&shim) {
            std::process::exit(code);
        }
    }
    let args: Vec<String> = match std::env::args_os()
        .skip(1)
        .map(std::ffi::OsString::into_string)
        .collect::<Result<_, _>>()
    {
        Ok(args) => args,
        Err(_) => std::process::exit(refuse(
            "command arguments must be valid Unicode; replace the non-Unicode argument and retry",
        )),
    };
    if let Some(code) = compose::external_host::probe(&args) {
        std::process::exit(code);
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(result) = grapt_sandbox::Bwrap::inner_main(&args) {
            if let Err(reason) = result {
                eprintln!("{reason}");
            }
            std::process::exit(1);
        }
        if let Some(code) = dogfood::sandbox_helper_main(&args) {
            std::process::exit(code);
        }
    }
    if args.first().is_some_and(|verb| verb == "sandbox-policy") {
        let code = match dogfood::sandbox_policy_preflight(&args[1..]) {
            Ok(report) => {
                println!("{report}");
                0
            }
            Err(reason) => refuse_as("grapt.sandbox-policy.v1", "sandbox policy", &reason),
        };
        std::process::exit(code);
    }
    if args.first().is_some_and(|verb| verb == "serve-ephemeral") {
        // This dispatch precedes installation/profile guards and ordinary
        // service admission. A volatile run must never acquire either.
        let result = if args.len() == 3 && args[1] == "--service" {
            grapt_service::launcher_environment::require_unbound()
                .map_err(str::to_owned)
                .and_then(|()| {
                    compose::ephemeral::run(std::path::Path::new(&args[2]))
                        .map_err(|error| error.to_string())
                })
        } else {
            Err("serve-ephemeral requires --service <read-only-provider-profile>".into())
        };
        let code = match result {
            Ok(()) => 0,
            Err(reason) => {
                // Readiness stdout is a private, single-line protocol. Refusals
                // must not resemble a readiness record or print its credential.
                crate::server_log::error(format_args!("ephemeral service refused: {reason}"));
                2
            }
        };
        std::process::exit(code);
    }
    if args.first().is_some_and(|verb| verb == "cleanup-sessions") {
        if args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h") {
            println!("{}", cleanup_command::HELP);
            std::process::exit(0);
        }
        let code = match grapt_service::launcher_environment::require_unbound()
            .map_err(str::to_owned)
            .and_then(|()| cleanup_command::run(&args[1..]))
        {
            Ok(report) => {
                println!("{report}");
                0
            }
            Err(reason) => refuse_as("grapt.session-retention.v1", "session cleanup", &reason),
        };
        std::process::exit(code);
    }
    if args.first().is_some_and(|verb| verb == "retire-service") {
        if args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h") {
            println!("{}", retire_service::HELP);
            std::process::exit(0);
        }
        let code = match grapt_service::launcher_environment::require_unbound()
            .map_err(str::to_owned)
            .and_then(|()| retire_service::run(&args[1..]))
        {
            Ok(receipt) => {
                println!("{receipt}");
                0
            }
            Err(reason) => refuse_as(
                "secode.product.service-retirement.v1",
                "service retirement",
                &reason,
            ),
        };
        std::process::exit(code);
    }
    if args == ["check-models"] {
        let code = match dogfood::first_use::check_chatgpt_models(&mut io::stdin().lock()) {
            Ok(()) => 0,
            Err(_) => refuse(
                "the account model listing or public catalog is incompatible; nothing was created",
            ),
        };
        std::process::exit(code);
    }
    if args
        .first()
        .is_some_and(|argument| argument == "check-profile")
    {
        let code = match installation::check(&args) {
            Ok(()) => {
                println!("profile is readable by this build");
                0
            }
            Err(error) => refuse(&error),
        };
        std::process::exit(code);
    }
    if args.first().is_some_and(|verb| {
        matches!(
            verb.as_str(),
            "serve" | "serve-service" | "serve-terminal" | "serve-connections" | "handover"
        )
    }) {
        if let Err(error) = grapt_service::launcher_environment::require_unbound() {
            std::process::exit(refuse(error));
        }
    }
    let _prefix_guard = match installation::guard() {
        Ok(guard) => guard,
        Err(error) => std::process::exit(refuse(&error)),
    };
    let code = match parse(&args) {
        Ok(Command::Usage {
            service_root,
            session,
            prices,
        }) => match crate::usage_accounting::run(&service_root, &session, prices.as_deref()) {
            Ok(report) => {
                println!("{report}");
                0
            }
            Err(error) => refuse_as("secode.session-accounting.v1", "session accounting", &error),
        },
        Ok(Command::InitEmptyProfile { service_root }) => {
            match dogfood::first_use::initialize_empty_profile(&service_root) {
                Ok(receipt) => {
                    println!("{receipt}");
                    0
                }
                Err(error) => refuse(&error),
            }
        }
        Ok(Command::InitProfile {
            service_root,
            source,
        }) => match dogfood::first_use::initialize_profile(&service_root, &source) {
            Ok(receipt) => {
                println!("{receipt}");
                0
            }
            Err(error) => refuse(&error),
        },
        Ok(Command::CodeModeWorker) => {
            // Private pipes only: no service/config/credential startup occurs.
            match grapt_codemode::run_worker(&mut io::stdin().lock(), &mut io::stdout().lock()) {
                Ok(()) => 0,
                Err(_) => {
                    crate::server_log::error("CodeMode worker protocol failed");
                    2
                }
            }
        }
        Ok(Command::Version) => {
            #[cfg(feature = "loopback-provider-testkit")]
            println!("{VERSION} (testkit/fixture: loopback-provider-testkit)");
            #[cfg(not(feature = "loopback-provider-testkit"))]
            println!("{VERSION}");
            0
        }
        Ok(Command::Startup {
            service_root,
            slot,
            environment,
            instance,
        }) => {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => match runtime.block_on(startup::StartupSnapshot::inspect(
                    &service_root,
                    &slot,
                    &environment,
                    &instance,
                )) {
                    Ok((generation, instance)) => {
                        println!(
                            "{}",
                            serde_json::json!({"schema":"grapt.product.startup.v1", "generation":generation, "instance":instance})
                        );
                        0
                    }
                    Err(error) => refuse(&error.to_string()),
                },
                Err(error) => refuse(&format!("startup observer runtime failed: {error}")),
            }
        }
        Ok(Command::Handover {
            service_root,
            run_root,
            run_id,
            engine_version,
        }) => match handover_request(&service_root, run_root, &run_id, engine_version) {
            Ok(request) => match handover::serve_after_handover(&service_root, request) {
                Ok(receipt) => match receipt.encoded() {
                    Ok(encoded) => {
                        println!("{encoded}");
                        0
                    }
                    Err(error) => refuse(&format!("launch receipt encoding failed: {error}")),
                },
                Err(error) => refuse(&error.to_string()),
            },
            Err(error) => refuse(&error),
        },
        Ok(Command::ServeConnections { service_root }) => {
            match compose::run_connections(&service_root) {
                Ok(_) => 0,
                Err(error) => refuse(&error.to_string()),
            }
        }
        Ok(Command::ServeTerminal {
            service_root,
            approval,
            notify_ready,
        }) => match approved_terminal(&service_root, approval.as_deref(), notify_ready) {
            Ok(receipt) => match receipt.encoded() {
                Ok(encoded) => {
                    println!("{encoded}");
                    0
                }
                Err(error) => refuse(&format!("launch receipt encoding failed: {error}")),
            },
            Err(error) => refuse(&error.to_string()),
        },
        Ok(Command::ServeService {
            service_root,
            external_host,
        }) => match if external_host {
            compose::external_host::run(&service_root)
        } else {
            compose::run_service(&service_root)
        } {
            Ok(receipt) => match receipt.encoded() {
                Ok(encoded) => {
                    println!("{encoded}");
                    0
                }
                Err(error) => refuse(&format!("launch receipt encoding failed: {error}")),
            },
            Err(error) => refuse(&error.to_string()),
        },
        Ok(Command::Serve { service_root }) => match compose::run(&service_root) {
            Ok(receipt) => match receipt.encoded() {
                Ok(encoded) => {
                    println!("{encoded}");
                    0
                }
                Err(error) => refuse(&format!("launch receipt encoding failed: {error}")),
            },
            Err(error) => refuse(&error.to_string()),
        },
        Ok(Command::ToolchainActivate { root, target, pin }) => {
            let clock = grapt_store::SystemClock;
            let source = grapt_toolchain::FileSource::new(root.clone());
            let request = grapt_toolchain::ActivationRequest {
                root,
                pin,
                target,
                platform: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                clock: &clock,
                source: &source,
            };
            match grapt_toolchain::activate(&request) {
                Ok(receipt) => {
                    println!("{}", receipt.json());
                    0
                }
                Err(refusal) => {
                    println!("{}", refusal.receipt_json());
                    crate::server_log::error(format_args!(
                        "toolchain activation refused: {refusal}"
                    ));
                    2
                }
            }
        }
        Err(error) => refuse(&error),
    };
    std::process::exit(code);
}

/// Build the handover request, refusing invalid inputs before anything runs.
///
/// The source is the installation's own database rather than a supplied path:
/// a handover that could be pointed at some other file would be a different
/// operation wearing this one's name.
fn handover_request(
    service_root: &std::path::Path,
    run_root: PathBuf,
    run_id: &str,
    engine_version: String,
) -> Result<handover::HandoverRequest, String> {
    if engine_version.trim().is_empty() {
        return Err(
            "--engine-version must be non-empty: it is stamped durably into the migrated \
             database and identifies the binary that did it"
                .to_owned(),
        );
    }
    Ok(handover::HandoverRequest {
        service_root: service_root.to_path_buf(),
        run_root,
        source: service_root.join("database.sqlite3"),
        run_id: grapt_types::MigrationId::new(run_id)
            .ok_or_else(|| "--run-id must be non-empty".to_owned())?,
        engine_version,
        // Thirty observations at 100ms: three seconds for an incumbent that is
        // already going down, and a bounded refusal for one that is not.
        retirement_attempts: 30,
        retirement_interval: std::time::Duration::from_millis(100),
    })
}

fn approved_terminal(
    service_root: &std::path::Path,
    approval: Option<&str>,
    notify_ready: bool,
) -> Result<LaunchReceipt, compose::ProductRefusal> {
    use grapt_platform::profile_configuration::{install, ProfileApproval};
    if let Some(encoded) = approval {
        let decision = ProfileApproval::decode(encoded).map_err(compose::ProductRefusal::Input)?;
        install(service_root, &decision).map_err(compose::ProductRefusal::Input)?;
        // All consumers use exactly the canonical root covered by the decision.
        compose::run_terminal(decision.canonical(), notify_ready)
    } else {
        compose::run_terminal(service_root, notify_ready)
    }
}

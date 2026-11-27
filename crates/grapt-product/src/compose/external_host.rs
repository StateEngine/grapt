//! Explicit service hosting for an independently owned StateEngine process.

use std::io::{self, Write};
use std::path::Path;

/// This probe must run before installation guards, profile reads or runtimes.
/// Even malformed probe requests stay on this side-effect-free path.
pub(crate) fn probe(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some("--external-host-capabilities") {
        return None;
    }
    if args.len() != 1 {
        let _result = writeln!(
            io::stderr(),
            "usage: grapt-server --external-host-capabilities"
        );
        return Some(2);
    }
    let capabilities = serde_json::json!({
        "schema": "secode.external-host-capabilities.v1",
        "external_host": true,
        "stateengine": {
            "owner": "external",
            "embedded_owner_composed": false,
            "configured_mcp_transports": "unchanged"
        },
        "service": {
            "argv": ["serve-service", "--service", "<profile>", "--port", "0", "--external-host"],
            "profile_init_argv": ["init-profile", "--service", "<new-absolute-profile>"],
            "readiness": {
                "file": crate::ready::READY_RECEIPT_FILE,
                "schema": grapt_api::product_health::PRODUCT_READY_SCHEMA,
                "endpoint_field": "observed_endpoint",
                "authenticated_health_path": "/api/health"
            },
            "shutdown": shutdown_signals()
        },
        "initial_input": {
            "cli_argument": "--input-id",
            "key_type": "MessageId",
            "acceptance_path": "/api/session/{sessionID}/input/{messageID}",
            "uncertain_write": "pending-no-replay"
        },
        "native_notice": {
            "binding_path": "/api/session/{sessionID}/stateengine/notice",
            "offer_method": "POST",
            "offer_path": "/api/session/{sessionID}/stateengine/notice",
            "acceptance_path": "/api/session/{sessionID}/input/{messageID}",
            "message_limit": 32,
            "body_delivery": false,
            "resume": {
                "method": "POST",
                "path": "/api/session/{sessionID}/stateengine/resume",
                "fences": ["service", "targetID", "binding", "holdID"],
                "admits_input": false,
                "starts_execution": false
            }
        }
    });
    Some(match writeln!(io::stdout().lock(), "{capabilities}") {
        Ok(()) => 0,
        Err(_) => 2,
    })
}

fn shutdown_signals() -> &'static [&'static str] {
    #[cfg(unix)]
    {
        &["SIGTERM", "SIGINT"]
    }
    #[cfg(not(unix))]
    {
        &["Ctrl+C"]
    }
}

pub(crate) fn run(
    service_root: &Path,
) -> Result<crate::receipt::LaunchReceipt, super::ProductRefusal> {
    // The existing service composition has no embedded StateEngine owner.
    // External mode also disables its legacy native-mail consumer. Configured
    // MCP transports still belong to the caller's standalone runtime.
    super::run_lifetime(
        service_root,
        super::Lifetime::Service,
        super::HostMode::External,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn external_host_is_an_explicit_service_option() {
        let mut args = [
            "serve-service",
            "--service",
            "fixture-profile",
            "--port",
            "0",
        ]
        .map(str::to_owned)
        .to_vec();
        assert!(matches!(
            crate::command::parse(&args),
            Ok(crate::command::Command::ServeService {
                external_host: false,
                ..
            })
        ));
        args.push("--external-host".into());
        assert!(matches!(
            crate::command::parse(&args),
            Ok(crate::command::Command::ServeService {
                external_host: true,
                ..
            })
        ));
        args.push("--external-host".into());
        assert!(crate::command::parse(&args).is_err());
        args.pop();
        args[0] = "serve".into();
        assert!(crate::command::parse(&args).is_err());
    }
}

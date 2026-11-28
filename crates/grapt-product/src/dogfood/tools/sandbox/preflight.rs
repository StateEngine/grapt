//! Read-only policy inspection before creating a service or Session.
use super::super::super::catalog_config::CatalogEnvironment;
use super::*;

const SCHEMA: &str = "grapt.sandbox-policy.v1";
const CAPABILITIES: [&str; 1] = ["git_metadata_read_only/v1"];

pub(crate) fn run(arguments: &[String]) -> Result<String, String> {
    let report = if arguments == ["--capabilities"] {
        serde_json::json!({"schema": SCHEMA, "capabilities": CAPABILITIES})
    } else if arguments.len() == 4 && arguments[0] == "--service" && arguments[2] == "--directory" {
        let service = Path::new(&arguments[1]);
        let directory = Path::new(&arguments[3]);
        if !service.is_absolute() || !directory.is_absolute() {
            return Err("sandbox policy requires absolute service and workspace paths".into());
        }
        let sources = CatalogSources::read(directory, CatalogEnvironment::read(service)?)?;
        let plan = Plan::read(directory, &sources)?;
        let policy = plan.map(|plan| {
            serde_json::json!({
                "git_metadata_read_only": plan.git_metadata_read_only,
                "readable_roots": plan.readable,
                "writable_roots": plan.writable,
                "protected_paths": plan.protected,
                "denied_paths": plan.denied,
                "network": plan.network,
            })
        });
        serde_json::json!({
            "schema": SCHEMA, "capabilities": CAPABILITIES,
            "policy": policy, "realization": "not_started",
        })
    } else {
        return Err("usage: sandbox-policy --capabilities | sandbox-policy --service <absolute-root> --directory <absolute-workspace>".into());
    };
    serde_json::to_string(&report)
        .map_err(|error| format!("sandbox policy encoding failed: {error}"))
}

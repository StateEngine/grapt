use super::*;
use grapt_codemode::{CatalogTool, ExecuteRequest};
use grapt_types::{ToolDefinitionId, ToolSchemaVersion};
mod worker;
pub(in crate::dogfood) use worker::WorkerRegistry;
pub(super) use worker::{run, Workers};

pub(super) fn prepare(
    published: &super::super::mcp::PublishedTools,
    input: &BoundedToolInput,
    nested: Option<grapt_runtime::NestedTools>,
) -> Result<PreparedEffect, String> {
    let args: ExecuteArguments = decode_input(input)?;
    let _ = args.description;
    let request = catalog(published, args.code.0)?;
    if request.tools.is_empty() {
        return Err("No CodeMode tools are available".into());
    }
    let nested = nested.ok_or("CodeMode requires supervised nested admission")?;
    let resource = format!(
        "{}:{}",
        request.catalog.as_str(),
        grapt_event::canonical::canonical_hash_hex(&serde_json::Value::String(
            request.code.clone()
        ))
    );
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::default(),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        target: EffectTarget::CodeMode { request, nested },
        capability: CapabilityId::derive(&resolved),
        action: EXECUTE_TOOL.to_owned(),
        resource,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct Source(String);

impl<'de> Deserialize<'de> for Source {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        let code = String::deserialize(decoder)?;
        if code.len() > grapt_codemode::SOURCE_BYTES {
            return Err(D::Error::custom("JavaScript source exceeds 64 KiB UTF-8"));
        }
        Ok(Self(code))
    }
}

impl ProviderFieldSchema for Source {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({"type":"string","maxLength":grapt_codemode::SOURCE_BYTES,
            "description":"JavaScript source; at most 64 KiB UTF-8."})
    }
}

tool_arguments! {
    struct ExecuteArguments => EXECUTE_TOOL {
        required code: Source,
        optional description: ToolDescription,
    }
}

/// The closed argument schema `execute` declares.
pub(super) fn argument_schema() -> String {
    ExecuteArguments::provider_schema()
}

// Copyright 2025 OpenAI. Apache-2.0.
// Modified by Grapt: fixed CodeMode discovery shell instead of a native
// Tool_search declaration.
pub(super) fn definition(
    published: &super::super::mcp::PublishedTools,
) -> Result<Option<ToolDefinition>, String> {
    let request = catalog(published, String::new())?;
    if request.tools.is_empty() {
        return Ok(None);
    }
    let mut schema: serde_json::Value = serde_json::from_str(&ExecuteArguments::provider_schema())
        .map_err(|_| "CodeMode argument schema encoding failed")?;
    schema["description"] = serde_json::Value::String(
        "Fresh bounded JavaScript guest for deferred MCP tools. Discover via catalog.search(query, limit) (ranked any-word search over names, descriptions and input schemas; default 8, max 16; omitted query lists names/short descriptions), then catalog.describe(name) for schemas. Built-in tools are called directly, outside this catalog. Call await tools[name](arguments) using the exact discovered name; return needed results. Catch individual rejected calls to inspect their {error,tool,message} refusal before deciding what to do next. Calls use ordinary approval. No imports, timers, filesystem, network or retained globals. Catalog queries execute no tools; unknown names are refused."
            .to_owned(),
    );
    Ok(Some(ToolDefinition {
        input_format: grapt_provider::ToolInputFormat::Json,
        name: EXECUTE_TOOL.to_owned(),
        description: Some(
            "Compose this execution's published tools with bounded JavaScript and ordinary approval."
                .to_owned(),
        ),
        input_schema: schema.to_string(),
    }))
}

fn catalog(
    published: &super::super::mcp::PublishedTools,
    code: String,
) -> Result<ExecuteRequest, String> {
    let mut tools = Vec::new();
    let mut identities = Vec::new();
    for definition in &published.definitions {
        let Some(route) = published.routes.get(&definition.name) else {
            continue;
        };
        if !published.codemode_servers.contains(&route.server) {
            continue;
        }
        if ["__proto__", "constructor", "prototype", EXECUTE_TOOL]
            .contains(&definition.name.as_str())
        {
            return Err("CodeMode catalog contains a reserved name".into());
        }
        // Every admitted name must fit 64 status rows in ordinary history,
        // including JSON escaping and room for the parent capture metadata.
        let row = serde_json::to_vec(&serde_json::json!({
            "tool":definition.name,"status":"completed"
        }))
        .map_err(|_| "CodeMode progress encoding failed")?;
        if row
            .len()
            .saturating_add(1)
            .saturating_mul(64)
            .saturating_add(1024)
            > grapt_api::u3a::limits::MAX_METADATA_BYTES
        {
            return Err("CodeMode tool name exceeds history progress capacity".into());
        }
        let input_schema = serde_json::from_str(&definition.input_schema)
            .map_err(|_| "CodeMode schema is invalid JSON")?;
        tools.push(CatalogTool {
            name: ToolDefinitionId::new(&definition.name)
                .ok_or("CodeMode tool identity invalid")?,
            description: route
                .description
                .clone()
                .unwrap_or_else(|| format!("Call {}", definition.name)),
            input_schema,
            output_schema: route
                .output_schema
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| "CodeMode output schema is invalid JSON")?,
        });
        let launch = grapt_event::canonical::canonical_hash_hex(&serde_json::json!({
            "program":route.launch.program,"arguments":route.launch.arguments,"cwd":route.launch.cwd,
            "incarnation":route.launch.incarnation}));
        identities.push(
            serde_json::json!({"name":definition.name,"server":route.server,
            "tool":route.tool.as_str(),"launch":launch,"outputSchema":route.output_schema}),
        );
    }
    let hash = grapt_event::canonical::canonical_hash_hex(
        &serde_json::json!({"tools":tools,"routes":identities}),
    );
    let request = ExecuteRequest {
        code,
        tools,
        catalog: ToolSchemaVersion::new(hash).ok_or("CodeMode catalog identity invalid")?,
    };
    request
        .validate()
        .map_err(|_| "CodeMode catalog exceeds its bound or contains duplicate names")?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn execute_arguments_own_schema_and_utf8_source_bound() -> Result<(), Box<dyn std::error::Error>>
    {
        let schema: serde_json::Value = serde_json::from_str(&ExecuteArguments::provider_schema())?;
        assert_eq!(schema["required"], serde_json::json!(["code"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["code"]["maxLength"], 65536);
        assert_eq!(schema["properties"]["description"]["type"], "string");
        assert_eq!(schema["properties"]["description"]["maxLength"], 160);
        let described: ExecuteArguments = serde_json::from_value(
            serde_json::json!({"code":"return 1", "description":"Read the current StateEngine assignment"}),
        )?;
        assert_eq!(described.code.0, "return 1");
        assert!(described.description.is_some());
        for code in [String::new(), "x".repeat(65536)] {
            let input = serde_json::json!({"code":code});
            assert!(serde_json::from_value::<ExecuteArguments>(input).is_ok());
        }
        for input in [
            serde_json::json!({}),
            serde_json::json!({"code":null}),
            serde_json::json!({"code":3}),
            serde_json::json!({"code":"return 1", "description":null}),
            serde_json::json!({"code":"return 1", "description":"x".repeat(161)}),
            serde_json::json!({"code":"return 1", "extra":true}),
            serde_json::json!({"code":"x".repeat(65537)}),
            serde_json::json!({"code":"é".repeat(32769)}),
        ] {
            assert!(serde_json::from_value::<ExecuteArguments>(input).is_err());
        }
        Ok(())
    }
    #[test]
    fn stateengine_resume_is_discoverable_without_changing_its_bound_route() -> Result<(), String> {
        let mut published = fixture()?;
        let mut route = published.routes.remove("fixture_add").ok_or("route")?;
        route.server = "stateengine".into();
        route.tool = grapt_mcp::McpToolName::new("stateengine_resume").ok_or("tool")?;
        let name = "stateengine_stateengine_resume";
        published.definitions[0].name = name.into();
        published.routes.insert(name.into(), route.clone());
        published.codemode_servers.clear();
        published.codemode_servers.insert("stateengine".into());
        let request = catalog(&published, String::new())?;
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name.as_str(), name);
        assert_eq!(published.routes.get(name), Some(&route));
        published.codemode_servers.clear();
        assert!(catalog(&published, String::new())?.tools.is_empty());
        Ok(())
    }

    #[test]
    fn empty_catalog_offers_no_execute() -> Result<(), String> {
        assert!(definition(&super::super::super::mcp::PublishedTools::default())?.is_none());
        Ok(())
    }

    fn fixture() -> Result<crate::dogfood::mcp::PublishedTools, String> {
        let mut published = crate::dogfood::mcp::PublishedTools::default();
        published.codemode_servers.insert("fixture".into());
        published.definitions.push(ToolDefinition {
            input_format: grapt_provider::ToolInputFormat::Json,
            name: "fixture_add".into(),
            description: Some("Add exact decimal operands".into()),
            input_schema: r#"{"type":"object","properties":{"a":{"type":"number"}}}"#.into(),
        });
        published.routes.insert(
            "fixture_add".into(),
            crate::dogfood::mcp::McpRoute {
                description: Some("Add exact decimal operands".into()),
                output_schema: Some(
                    r#"{"type":"object","properties":{"sum":{"type":"number"}}}"#.into(),
                ),
                server: "fixture".into(),
                tool: grapt_mcp::McpToolName::new("add").ok_or("tool")?,
                launch: crate::dogfood::mcp::LaunchIdentity {
                    declaration: std::sync::Arc::new(crate::dogfood::mcp::Declaration::Disabled),
                    program: "fixture".into(),
                    arguments: Vec::new(),
                    cwd: "/fixture".into(),
                    incarnation: 1,
                },
            },
        );
        Ok(published)
    }

    #[test]
    fn catalog_preserves_declared_discovery_and_changes_identity_with_its_sources(
    ) -> Result<(), String> {
        let published = fixture()?;
        let original = catalog(&published, String::new())?;
        assert_eq!(original.tools[0].description, "Add exact decimal operands");
        assert_eq!(
            original.tools[0].output_schema,
            Some(serde_json::json!({
                "type":"object","properties":{"sum":{"type":"number"}}
            }))
        );
        let advertised = definition(&published)?
            .ok_or("execute missing")?
            .input_schema;
        assert!(!advertised.contains("Add exact decimal operands"));
        assert!(!advertised.contains("fixture_add") && !advertised.contains("sum"));
        assert!(advertised.contains("catalog.search") && advertised.contains("catalog.describe"));
        assert!(advertised.len() < 2048);
        for changed in ["input", "output", "description", "launch"] {
            let mut next = fixture()?;
            let route = next.routes.get_mut("fixture_add").ok_or("route")?;
            match changed {
                "input" => next.definitions[0].input_schema = "{}".into(),
                "output" => route.output_schema = Some("{}".into()),
                "description" => route.description = Some("Changed meaning".into()),
                "launch" => route.launch.incarnation = 2,
                _ => return Err("unknown fixture change".into()),
            }
            assert_eq!(
                definition(&next)?.ok_or("execute missing")?.input_schema,
                advertised,
                "live {changed} must not rewrite the fixed discovery prefix"
            );
            assert_ne!(
                catalog(&next, String::new())?.catalog,
                original.catalog,
                "{changed}"
            );
        }
        Ok(())
    }

    #[test]
    fn catalog_refuses_malformed_duplicate_and_reserved_discovery() -> Result<(), String> {
        for output in ["not-json", "null", "true", "[]"] {
            let mut published = fixture()?;
            published
                .routes
                .get_mut("fixture_add")
                .ok_or("route")?
                .output_schema = Some(output.into());
            assert!(catalog(&published, String::new()).is_err());
        }
        let mut duplicate = fixture()?;
        duplicate.definitions.push(duplicate.definitions[0].clone());
        assert!(catalog(&duplicate, String::new()).is_err());
        for reserved in ["execute", "__proto__", "constructor", "prototype"] {
            let mut published = fixture()?;
            let route = published.routes.remove("fixture_add").ok_or("route")?;
            published.definitions[0].name = reserved.into();
            published.routes.insert(reserved.into(), route);
            assert!(catalog(&published, String::new()).is_err());
        }
        Ok(())
    }

    #[test]
    fn published_schema_limit_with_escaped_descriptions_fits_the_worker_frame(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let sample = fixture()?;
        let mut published = super::super::super::mcp::PublishedTools::default();
        published.codemode_servers.insert("fixture".into());
        let description = "\0".repeat(512);
        let mut schema = serde_json::json!({"type":"object", "description":""});
        let per_schema = super::super::super::mcp::MAX_PUBLISHED_SCHEMA_BYTES
            / grapt_codemode::CATALOG_TOOLS
            - description.len();
        let padding = per_schema - schema.to_string().len();
        schema["description"] = serde_json::json!("x".repeat(padding));
        let mut carried = 0;
        for index in 0..grapt_codemode::CATALOG_TOOLS {
            let name = format!("fixture_tool_{index}");
            let mut definition = sample.definitions[0].clone();
            definition.name = name.clone();
            definition.description = Some(description.clone());
            definition.input_schema = schema.to_string();
            carried += definition.input_schema.len() + description.len();
            published.definitions.push(definition);
            let mut route = sample.routes["fixture_add"].clone();
            route.description = Some(description.clone());
            route.output_schema = None;
            published.routes.insert(name, route);
        }
        assert_eq!(
            carried,
            super::super::super::mcp::MAX_PUBLISHED_SCHEMA_BYTES
        );
        let request = catalog(&published, "\0".repeat(grapt_codemode::SOURCE_BYTES))?;
        let mut bytes = Vec::new();
        grapt_codemode::write_frame(&mut bytes, &request)?;
        assert!(
            bytes.len() > 256 * 1024,
            "old catalog boundary not exercised"
        );
        assert!(bytes.len() <= grapt_codemode::FRAME_BYTES + 4);
        let decoded: ExecuteRequest = grapt_codemode::read_frame(&mut std::io::Cursor::new(bytes))?;
        assert_eq!(decoded.tools.len(), grapt_codemode::CATALOG_TOOLS);
        assert_eq!(decoded.tools[255].description, description);
        assert_eq!(decoded.tools[255].input_schema, schema);
        Ok(())
    }
}

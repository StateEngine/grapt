//! Configuration reading, MCP declaration shape, and the catalog projection.
//!
//! Split out of `connections.rs` under the workspace god-file ceiling.
use super::*;
#[path = "tests/codemode_defaults.rs"]
mod codemode_defaults;

fn withheld(server: &str, name: &str) -> super::super::mcp::WithheldTool {
    super::super::mcp::WithheldTool {
        server: server.to_owned(),
        name: name.to_owned(),
    }
}

/// EVERY ROW THIS PRODUCT EMITS MUST DECODE AS THE CLOSED DTO.
///
/// `Mcp.Server` carries name, status and integrationID and the decoder
/// refuses unknown fields, so ONE extra key turns the whole catalog into
/// an InvalidRequest — the user loses the server list as well as whatever
/// the key was trying to tell them. I shipped exactly that regression by
/// adding an `omitted` field; this drives the real `servers()` output
/// through the real decoder so it cannot come back unnoticed.
#[test]
fn every_emitted_server_row_decodes_as_the_closed_dto() {
    let snapshot = McpSnapshot {
        roots: Default::default(),
        project_servers: BTreeMap::new(),
        declared: BTreeMap::new(),
        scope: BTreeMap::new(),
        codemode_servers: std::collections::BTreeSet::new(),
        blocked: Vec::new(),
    };
    let mut entries = BTreeMap::new();
    entries.insert(
        "disabled-one".to_owned(),
        super::super::mcp::ServerEntry::Disabled,
    );
    entries.insert(
        "failed-one".to_owned(),
        super::super::mcp::ServerEntry::unusable(grapt_mcp::session::Unusable::TimedOut),
    );

    for row in snapshot.servers(&entries) {
        let decoded: Result<grapt_api::u3a::McpServerDto, String> = decode(row.clone());
        assert!(
            decoded.is_ok(),
            "an emitted row must decode as Mcp.Server: {row} -> {decoded:?}"
        );
    }
}

/// The three omission reasons stay DISTINCT on the operator surface,
/// because they need three different actions: rename the tool, rename one
/// of the two servers, or declare fewer of them.
#[test]
fn a_withheld_tool_is_reported_with_the_reason_it_was_withheld() {
    let published = super::super::mcp::PublishedTools {
        shadowed: vec![withheld("srv", "srv_read")],
        ambiguous: vec![withheld("srv", "srv_dup")],
        unpublished: vec![withheld("srv", "srv_extra")],
        ..Default::default()
    };
    let rows = super::omissions("srv", &published);
    let reasons: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.get("reason").and_then(Value::as_str))
        .collect();
    assert_eq!(rows.len(), 3);
    assert!(reasons[0].contains("built-in"));
    assert!(reasons[1].contains("another configured server"));
    assert!(reasons[2].contains("bound"));
}

/// Another server's withheld tool is not reported against this one. An
/// operator reading a row must be reading about the server it names.
#[test]
fn one_servers_omissions_are_not_reported_against_another() {
    let published = super::super::mcp::PublishedTools {
        ambiguous: vec![withheld("other", "other_dup")],
        ..Default::default()
    };
    assert!(super::omissions("srv", &published).is_empty());
}

/// A server whose name needed sanitising still matches its own tools. The
/// prefix is built with the SAME rule the qualified name was, so a name
/// with a space or a slash is not silently unreportable.
#[test]
fn a_sanitised_server_name_still_matches_its_own_withheld_tools() {
    let published = super::super::mcp::PublishedTools {
        shadowed: vec![withheld("my server", "my_server_read")],
        ..Default::default()
    };
    assert_eq!(super::omissions("my server", &published).len(), 1);
}

/// A connected server with nothing withheld carries no omission field at
/// all. An empty list and an absent one are different claims, and only the
/// absent one is true when nothing was withheld.
#[test]
fn a_server_with_nothing_withheld_reports_no_omissions() {
    assert!(super::omissions("srv", &super::super::mcp::PublishedTools::default()).is_empty());
}

/// Read one `grapt.json` body and return the declarations, or the
/// refusal, in an isolated project.
fn declarations_from(
    body: &str,
) -> Result<BTreeMap<String, super::super::mcp::Declaration>, String> {
    snapshot_from(body).map(|snapshot| snapshot.declared)
}

fn snapshot_from(body: &str) -> Result<McpSnapshot, String> {
    let root = std::env::temp_dir().join(format!(
        "grapt-mcp-config-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let project = root.join("project");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&project).map_err(|error| error.to_string())?;
    // The profile's own configuration: a project's local servers are not
    // started (see a_project_may_not_start_a_local_server).
    let service = root.join("service");
    std::fs::create_dir_all(&service).map_err(|error| error.to_string())?;
    std::fs::write(service.join("grapt.json"), body).map_err(|error| error.to_string())?;
    let environment = CatalogEnvironment::isolated(&root);
    let sources = CatalogSources::read(&project, environment).map_err(|error| error.to_string())?;
    let read = McpSnapshot::read(&sources, &project.to_string_lossy());
    let _ = std::fs::remove_dir_all(&root);
    read
}

fn local_fields(
    declared: &BTreeMap<String, super::super::mcp::Declaration>,
    name: &str,
) -> (
    String,
    BTreeMap<String, String>,
    super::super::mcp::McpTimeouts,
) {
    match declared.get(name) {
        Some(super::super::mcp::Declaration::Local {
            cwd,
            environment,
            timeouts,
            ..
        }) => (cwd.clone(), environment.clone(), *timeouts),
        other => panic!("{name} is not a local declaration: {other:?}"),
    }
}

/// THE DOCUMENTED SHAPE IS READ AT ALL.
///
/// `mcp: { servers, timeout }` is what the frozen contract defines and
/// what a user following it writes. This product read `mcp` as the server
/// map directly, so that config was taken as two servers named `servers`
/// and `timeout` and refused for declaring no type -- blaming the user's
/// server names for a shape this product did not implement.
#[test]
fn the_documented_mcp_shape_is_read() {
    let declared = declarations_from(
            r#"{"mcp":{"timeout":{"startup":1234},"servers":{"srv":{"type":"local","command":["x"]}}}}"#,
        )
        .unwrap_or_else(|error| panic!("the documented shape must be read: {error}"));
    let (_, _, timeouts) = local_fields(&declared, "srv");
    assert_eq!(timeouts.startup, std::time::Duration::from_millis(1234));
}

/// The flat shape this product has always accepted still works, so the
/// compatibility fix is not a migration.
#[test]
fn the_flat_shape_this_product_accepted_still_works() {
    let declared = declarations_from(r#"{"mcp":{"srv":{"type":"local","command":["x"]}}}"#)
        .unwrap_or_else(|error| panic!("the flat shape must still work: {error}"));
    assert!(declared.contains_key("srv"));
}

/// A STRAY KEY BESIDE THE DOCUMENTED SHAPE IS REFUSED, naming where it
/// belongs.
///
/// Once `servers` is present the node is read in the documented shape, so
/// a server left at the top level would otherwise be silently ignored --
/// configured, accepted, and never started.
#[test]
fn a_server_left_beside_the_documented_shape_is_refused() {
    let refusal = declarations_from(
            r#"{"mcp":{"servers":{"a":{"type":"local","command":["x"]}},"b":{"type":"local","command":["y"]}}}"#,
        )
        .err()
        .unwrap_or_else(|| panic!("a stray key must refuse"));
    assert!(
        refusal.contains("move `b` under `mcp.servers`"),
        "{refusal}"
    );
}

/// PER-SERVER OVERRIDES THE GLOBAL, and the global still supplies what the
/// server did not name. A per-server value replacing the whole global
/// would be a different contract.
#[test]
fn a_per_server_timeout_overrides_only_the_phase_it_names() {
    let declared = declarations_from(
            r#"{"mcp":{"timeout":{"startup":1000,"catalog":2000},
                      "servers":{"srv":{"type":"local","command":["x"],"timeout":{"catalog":9000}}}}}"#,
        )
        .unwrap_or_else(|error| panic!("layered timeouts must parse: {error}"));
    let (_, _, timeouts) = local_fields(&declared, "srv");
    assert_eq!(timeouts.startup, std::time::Duration::from_millis(1000));
    assert_eq!(timeouts.catalog, std::time::Duration::from_millis(9000));
}

/// A RELATIVE cwd RESOLVES AGAINST THE WORKSPACE, and an absent one is the
/// workspace itself.
#[test]
fn a_relative_cwd_resolves_against_the_location() {
    let declared =
        declarations_from(r#"{"mcp":{"srv":{"type":"local","command":["x"],"cwd":"nested"}}}"#)
            .unwrap_or_else(|error| panic!("a relative cwd must parse: {error}"));
    let (cwd, _, _) = local_fields(&declared, "srv");
    assert!(cwd.ends_with("nested"), "{cwd}");
    assert!(cwd.len() > "nested".len(), "cwd was not resolved: {cwd}");
}

#[test]
fn a_configured_environment_is_carried_into_the_declaration() {
    let declared = declarations_from(
        r#"{"mcp":{"srv":{"type":"local","command":["x"],"environment":{"A":"1"}}}}"#,
    )
    .unwrap_or_else(|error| panic!("a configured environment must parse: {error}"));
    let (_, environment, _) = local_fields(&declared, "srv");
    assert_eq!(environment.get("A").map(String::as_str), Some("1"));
}

/// A LATER DOCUMENT REPLACES THE WHOLE DECLARATION.
///
/// The omission case, which a changed-field witness cannot reach: the
/// second document declares only a `command`, and the first document's
/// `environment` and `cwd` must NOT survive into it. Merging per key kept
/// them, so a replacement declaration silently carried the credentials and
/// working directory it was written to replace.
#[test]
fn a_redeclared_server_does_not_inherit_the_fields_it_omitted() {
    let root = std::env::temp_dir().join(format!(
        "grapt-mcp-precedence-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let project = root.join("project");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&project).unwrap_or_else(|error| panic!("project directory: {error}"));
    // The PROFILE's document declares the server with an environment and
    // cwd; the user config directory, read after it, redeclares it with a
    // command only. Both are the user's own, so the server is started.
    let service = root.join("service");
    let config = root.join("config");
    for directory in [&service, &config] {
        std::fs::create_dir_all(directory)
            .unwrap_or_else(|error| panic!("config directory: {error}"));
    }
    std::fs::write(
            service.join("grapt.json"),
            br#"{"mcp":{"srv":{"type":"local","command":["a"],"environment":{"TOKEN_NAME":"one"},"cwd":"first"}}}"#,
        )
        .unwrap_or_else(|error| panic!("profile config: {error}"));
    std::fs::write(
        config.join("grapt.json"),
        br#"{"mcp":{"srv":{"type":"local","command":["b"]}}}"#,
    )
    .unwrap_or_else(|error| panic!("user config: {error}"));
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(&root))
        .unwrap_or_else(|error| panic!("catalog sources: {error}"));
    let declared = McpSnapshot::read(&sources, &project.to_string_lossy())
        .unwrap_or_else(|error| panic!("declarations must parse: {error}"))
        .declared;
    let (cwd, environment, _) = local_fields(&declared, "srv");
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        environment.is_empty(),
        "the replaced declaration inherited an environment it did not declare: {environment:?}"
    );
    assert!(
        !cwd.ends_with("first"),
        "the replaced declaration inherited a working directory it did not declare: {cwd}"
    );
}

/// A refusal names the document that holds the fault,
/// not the first one that mentions MCP. The profile's document declares a
/// good server; the user config directory, read after it, holds the bad
/// declaration (and, separately, a malformed `mcp` node).
#[test]
fn a_refusal_names_the_document_that_holds_the_fault() {
    let root = std::env::temp_dir().join(format!(
        "grapt-mcp-attribution-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let project = root.join("project");
    let service = root.join("service");
    let config = root.join("config");
    let refusal = |second: &str| -> String {
        let _ = std::fs::remove_dir_all(&root);
        for directory in [&project, &service, &config] {
            std::fs::create_dir_all(directory)
                .unwrap_or_else(|error| panic!("config directory: {error}"));
        }
        std::fs::write(
            service.join("grapt.json"),
            br#"{"mcp":{"good":{"type":"local","command":["a"]}}}"#,
        )
        .unwrap_or_else(|error| panic!("profile config: {error}"));
        std::fs::write(config.join("grapt.json"), second)
            .unwrap_or_else(|error| panic!("user config: {error}"));
        let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(&root))
            .unwrap_or_else(|error| panic!("catalog sources: {error}"));
        match McpSnapshot::read(&sources, &project.to_string_lossy()) {
            Ok(_) => panic!("the bad declaration was accepted"),
            Err(refusal) => refusal,
        }
    };
    let declared = refusal(r#"{"mcp":{"bad":{"command":["b"]}}}"#);
    let node = refusal(r#"{"mcp":5}"#);
    let _ = std::fs::remove_dir_all(&root);
    let second = config.join("grapt.json").display().to_string();
    let first = service.join("grapt.json").display().to_string();
    for (label, refusal, reason) in [
        ("declaration", &declared, "requires a local or remote type"),
        ("mcp node", &node, "MCP declarations must be an object"),
    ] {
        assert!(refusal.contains(reason), "{label}: {refusal}");
        assert!(
            refusal.ends_with(&second),
            "{label} did not name the second document: {refusal}"
        );
        assert!(
            !refusal.contains(&first),
            "{label} named the first: {refusal}"
        );
    }
}

/// EQUIVALENT SPELLINGS OF ONE DIRECTORY ARE ONE SERVER.
///
/// Identity is compared on the stored string, so `.` and `a/../.` and an
/// absent `cwd` must all produce the same value or an edit between them
/// would retire a healthy connection and invalidate a standing approval.
#[test]
fn equivalent_cwd_spellings_normalise_to_one_directory() {
    let absent = declarations_from(r#"{"mcp":{"s":{"type":"local","command":["x"]}}}"#)
        .unwrap_or_else(|error| panic!("absent cwd: {error}"));
    let dot = declarations_from(r#"{"mcp":{"s":{"type":"local","command":["x"],"cwd":"."}}}"#)
        .unwrap_or_else(|error| panic!("dot cwd: {error}"));
    let round_trip =
        declarations_from(r#"{"mcp":{"s":{"type":"local","command":["x"],"cwd":"nested/../."}}}"#)
            .unwrap_or_else(|error| panic!("round trip cwd: {error}"));
    let (a, _, _) = local_fields(&absent, "s");
    let (b, _, _) = local_fields(&dot, "s");
    let (c, _, _) = local_fields(&round_trip, "s");
    assert_eq!(a, b, "`.` must name the same directory as an absent cwd");
    assert_eq!(a, c, "`nested/../.` must fold to the same directory");
}

/// AND A GENUINELY DIFFERENT DIRECTORY STILL DIFFERS.
///
/// Without this the normalisation above is satisfied by folding every path
/// to one value.
#[test]
fn a_different_cwd_remains_a_different_directory() {
    let here = declarations_from(r#"{"mcp":{"s":{"type":"local","command":["x"],"cwd":"."}}}"#)
        .unwrap_or_else(|error| panic!("here: {error}"));
    let nested =
        declarations_from(r#"{"mcp":{"s":{"type":"local","command":["x"],"cwd":"nested"}}}"#)
            .unwrap_or_else(|error| panic!("nested: {error}"));
    let (a, _, _) = local_fields(&here, "s");
    let (b, _, _) = local_fields(&nested, "s");
    assert_ne!(a, b, "two different directories folded into one");
}

/// THE AMBIGUOUS FLAT DECLARATION IS REACHED AND REFUSED, naming where to
/// move it.
///
/// The first version of this rule keyed on the PRESENCE of `servers`, so a
/// flat server called `servers` made the key present, took the nested path,
/// and the guard written for it could never fire. Reading the value is what
/// makes the case reachable.
/// A NESTED SERVER MAY BE NAMED `type`, and the frozen `Schema.Record`
/// allows it. Paired with the ambiguity refusal so a blanket rejection of
/// every map carrying a `type` key cannot satisfy both.
#[test]
fn a_nested_server_may_be_named_type() {
    let declared =
        declarations_from(r#"{"mcp":{"servers":{"type":{"type":"local","command":["x"]}}}}"#)
            .unwrap_or_else(|error| panic!("a server named `type` is valid: {error}"));
    assert!(declared.contains_key("type"), "{declared:?}");
}

#[test]
fn a_flat_server_named_servers_is_refused_and_told_where_to_move() {
    let refusal = declarations_from(r#"{"mcp":{"servers":{"type":"local","command":["x"]}}}"#)
        .err()
        .unwrap_or_else(|| panic!("an ambiguous flat declaration must refuse"));
    assert!(refusal.contains("mcp.servers.servers"), "{refusal}");
}

#[test]
fn code_mode_true_and_false_are_valid_declarations() {
    let declared =
        declarations_from(r#"{"mcp":{"s":{"type":"local","command":["x"],"codemode":true}}}"#)
            .unwrap_or_else(|error| panic!("explicit CodeMode declaration: {error}"));
    assert!(declared.contains_key("s"));

    // `false` IS what this product does, so it is honoured rather than
    // refused. Without this the rule above is satisfied by rejecting the
    // key outright, which would break a correct configuration.
    let declared =
        declarations_from(r#"{"mcp":{"s":{"type":"local","command":["x"],"codemode":false}}}"#)
            .unwrap_or_else(|error| panic!("`codemode: false` matches this product: {error}"));
    assert!(declared.contains_key("s"));
}

/// EVERY MALFORMED VALUE REFUSES, NAMING WHAT IS WRONG.
///
/// This is the whole point of the change. Before it, each of these was
/// ACCEPTED and discarded: the user's configuration parsed, their intent
/// vanished, and nothing said so. A test asserting only that valid values
/// work would leave that defect exactly where it was.
#[test]
fn malformed_values_refuse_rather_than_disappearing() {
    for (body, expected) in [
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"timeout":{"startup":0}}}}"#,
            "positive",
        ),
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"timeout":{"startup":-5}}}}"#,
            "whole number",
        ),
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"timeout":{"startup":"30000"}}}}"#,
            "whole number",
        ),
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"timeout":{"startup":999999999}}}}"#,
            "bound",
        ),
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"timeout":{"warmup":10}}}}"#,
            "no warmup phase",
        ),
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"timeout":5}}}"#,
            "must be an object",
        ),
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"cwd":5}}}"#,
            "cwd must be a string",
        ),
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"cwd":""}}}"#,
            "must not be empty",
        ),
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"environment":[]}}}"#,
            "must be an object",
        ),
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"environment":{"A":5}}}}"#,
            "must be a string",
        ),
        (
            r#"{"mcp":{"s":{"type":"local","command":["x"],"environment":{"A=B":"1"}}}}"#,
            "invalid character",
        ),
    ] {
        let refusal = declarations_from(body)
            .err()
            .unwrap_or_else(|| panic!("this must refuse: {body}"));
        assert!(
            refusal.contains(expected),
            "refusal for {body} should mention {expected:?} but was {refusal:?}"
        );
    }
}

#[tokio::test]
async fn a_configured_server_is_actually_started_and_its_real_state_reported(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-mcp-catalog-{}", std::process::id()));
    let project = root.join("project");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(root.join("service"))?;
    let path = root.join("service").join("grapt.json");
    // "missing-server" names a program that does not exist, so the start
    // genuinely fails rather than being assumed to fail.
    std::fs::write(&path, br#"{"mcp":{"enabled":{"type":"local","command":["grapt-no-such-mcp-server"]},"disabled":{"disabled":true}}}"#)?;
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(&root))?;
    let snapshot = McpSnapshot::read(&sources, &project.to_string_lossy())?;
    assert_eq!(snapshot.declared.len(), 2);
    let registry = super::super::mcp::registry(std::sync::Arc::new(grapt_store::SystemClock))?;
    let servers = registry
        .with_status(&project.to_string_lossy(), &snapshot.declared, |entries| {
            snapshot.servers(entries)
        })
        .await;
    assert_eq!(servers.len(), 2);
    // Declaration order is not status order; find each by name.
    let by_name = |name: &str| -> Value {
        match servers.iter().find(|row| row["name"] == name) {
            Some(row) => row.clone(),
            None => panic!("{name} is absent from the status projection"),
        }
    };
    assert_eq!(by_name("disabled")["status"]["status"], "disabled");
    let failed = by_name("enabled");
    assert_eq!(failed["status"]["status"], "failed");
    let detail = failed["status"]["error"].as_str().unwrap_or_default();
    assert!(
        detail.contains("failed"),
        "the failure must say which state it is in: {detail}"
    );

    std::fs::write(
        &path,
        br#"{"mcp":{"broken":{"type":"local","command":null}}}"#,
    )?;
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(&root))?;
    assert!(
        McpSnapshot::read(&sources, &project.to_string_lossy()).is_err(),
        "a malformed declaration must refuse, not become an empty catalog"
    );
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// A cloned repository may not make SE spawn a command (ruling
/// Q2): a local server its configuration declares, or redeclares over the
/// user's, is validated and then not started, and is named for the
/// Session's notice. Project-declared remote servers are blocked too; the
/// user's own server declarations may connect.
#[test]
fn a_project_may_not_start_a_local_server() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!(
        "grapt-mcp-project-local-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let project = root.join("project");
    let service = root.join("service");
    let _ = std::fs::remove_dir_all(&root);
    for directory in [&project, &service] {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        service.join("grapt.json"),
        br#"{"mcp":{"mine":{"type":"local","command":["mine"]},"shadowed":{"type":"local","command":["user"]}}}"#,
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(
        project.join("grapt.json"),
        br#"{"mcp":{"repo":{"type":"local","command":["repo"],"codemode":true},"shadowed":{"type":"local","command":["repo"]},"web":{"type":"remote","url":"https://mcp.example.test/"}}}"#,
    )
    .map_err(|error| error.to_string())?;
    let sources = CatalogSources::read(&project, CatalogEnvironment::isolated(&root))?;
    let snapshot = McpSnapshot::read(&sources, &project.to_string_lossy());
    let _ = std::fs::remove_dir_all(&root);
    let snapshot = snapshot?;
    assert_eq!(
        snapshot.blocked,
        vec!["repo".to_owned(), "shadowed".to_owned(), "web".to_owned()]
    );
    assert!(snapshot.declared.contains_key("mine"));
    assert!(!snapshot.declared.contains_key("web"));
    assert!(!snapshot.declared.contains_key("repo"));
    assert!(!snapshot.declared.contains_key("shadowed"));
    assert!(!snapshot.codemode_servers.contains("repo"));
    Ok(())
}

#[test]
fn native_inspection_preserves_closed_server_status_and_reports_tool_omissions(
) -> Result<(), String> {
    let snapshot = McpSnapshot {
        roots: Default::default(),
        project_servers: BTreeMap::new(),
        declared: BTreeMap::new(),
        scope: BTreeMap::new(),
        codemode_servers: Default::default(),
        blocked: Vec::new(),
    };
    let entries = BTreeMap::from([(
        "disabled".to_owned(),
        super::super::mcp::ServerEntry::Disabled,
    )]);
    let rows = snapshot.inspection(&entries)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["server"]["status"]["status"], "disabled");
    assert_eq!(rows[0]["tools"], json!([]));
    let _: grapt_api::u3a::McpInspectionServerDto = decode(rows[0].clone())?;
    let _: grapt_api::u3a::McpServerDto = decode(rows[0]["server"].clone())?;
    let published = super::super::mcp::PublishedTools {
        shadowed: vec![withheld("srv", "srv_read"), withheld("other", "other_read")],
        ..Default::default()
    };
    let tools = super::inspection_tools("srv", &published);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "srv_read");
    assert!(tools[0]["omission"]
        .as_str()
        .ok_or("omission")?
        .contains("built-in"));
    let _: grapt_api::u3a::McpInspectionToolDto = decode(tools[0].clone())?;
    Ok(())
}

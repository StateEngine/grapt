//! Naming, collision refusal and the catalog bounds.

use serde_json::{json, Value};

use super::super::publication::{
    qualified_tool_name, shadows_builtin, tool_definitions_from, ConnectedCatalog,
    MAX_PUBLISHED_SCHEMA_BYTES, MAX_PUBLISHED_TOOLS,
};
use super::fixtures::*;
use grapt_mcp::accept_tools;

#[test]
fn an_mcp_description_reaches_the_model_facing_definition_unchanged() {
    let catalog = match accept_tools(
        bounds(),
        &json!({"tools":[{
            "name":"lookup",
            "description":"Look up one exact record",
            "inputSchema":{"type":"object","properties":{}}
        }]}),
    ) {
        Ok(catalog) => catalog,
        Err(error) => panic!("catalog: {error}"),
    };
    let connected = [connected_catalog("records", catalog.as_slice())];
    let published = tool_definitions_from(&connected, &["read", "edit", "shell"]);
    assert_eq!(published.definitions.len(), 1);
    assert_eq!(
        published.definitions[0].description.as_deref(),
        Some("Look up one exact record")
    );
    assert_eq!(
        published.routes["records_lookup"].description.as_deref(),
        Some("Look up one exact record")
    );
}

/// The qualified name is the ORACLE's, byte for byte.
///
/// The oracle's `mcp.ts:20-21`: `${namespace(server)}_${tool}`,
/// every byte outside `[a-zA-Z0-9_-]` replaced by `_`, in both halves.
/// This is not a convention worth reinventing — `mcp.ts:114` uses the same
/// string as the permission action, so a divergence here would let a user
/// approve one name while a different one was shown or recorded.
#[test]
fn a_qualified_tool_name_matches_the_oracles_rule_in_both_halves() {
    assert_eq!(
        qualified_tool_name("fixture", "read_file"),
        "fixture_read_file"
    );
    assert_eq!(
        qualified_tool_name("my server", "do.thing"),
        "my_server_do_thing",
        "a space and a dot are both outside the admitted set"
    );
    assert_eq!(
        qualified_tool_name("keep-dash", "keep_under"),
        "keep-dash_keep_under",
        "dash and underscore are admitted and must survive unchanged"
    );
    assert_eq!(
        qualified_tool_name("a/b", "c:d"),
        "a_b_c_d",
        "a path separator or colon in a name would read as structure it does not have"
    );
}

/// Qualified names that collide with the real built-in catalog are withheld.
#[test]
fn a_qualified_name_that_shadows_a_builtin_is_withheld() {
    let builtins = crate::dogfood::tools::BUILTIN_TOOL_NAMES;
    let qualified = qualified_tool_name("read", "lines");
    assert_eq!(qualified, "read_lines");
    assert!(shadows_builtin(&qualified, builtins));

    let catalog = catalog_of(&["lines", "notes"]);
    let connected = [connected_catalog("read", catalog.as_slice())];
    let published = tool_definitions_from(&connected, builtins);
    let names: Vec<_> = published
        .definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    assert_eq!(names, ["read_notes"]);
    assert_eq!(published.shadowed.len(), 1);
    assert_eq!(published.shadowed[0].server, "read");
    assert_eq!(published.shadowed[0].name, "read_lines");
    assert!(!published.routes.contains_key("read_lines"));
    assert!(published.routes.contains_key("read_notes"));
    assert!(published.ambiguous.is_empty());
    assert!(published.unpublished.is_empty());

    assert!(shadows_builtin("git_status", &["read", "git_status"]));
}

/// The qualified form is NOT injective, and the collision needs no
/// punctuation to happen: server `a_b` + tool `c` and server `a` + tool
/// `b_c` both yield `a_b_c`.
///
/// Both are refused. Publishing one would pick a winner by iteration
/// order, and this name is the permission ACTION — a user would approve
/// `a_b_c` believing one server while the other's tool ran. Equal public
/// strings do not establish equal authority.
#[test]
fn two_servers_claiming_one_public_name_publish_neither() {
    let ab_c = catalog_of(&["c"]);
    let a_bc = catalog_of(&["b_c"]);
    let connected: Vec<ConnectedCatalog<'_>> = vec![
        connected_catalog("a_b", ab_c.as_slice()),
        connected_catalog("a", a_bc.as_slice()),
    ];

    let published = tool_definitions_from(&connected, &["read", "edit", "shell"]);
    let ambiguous: Vec<&str> = published
        .ambiguous
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(
        ambiguous,
        vec!["a_b_c", "a_b_c"],
        "BOTH claimants are recorded: an operator must rename one and              cannot be told which without being told about both"
    );
    assert!(
        published.definitions.is_empty(),
        "neither claimant is published: picking one would pick by iteration order"
    );
}

/// Sanitisation creates collisions too: `a/b` and `a_b` are one prefix
/// after it. The refusal must not depend on the ORDER the servers appear
/// in, so the same pair is checked both ways round.
#[test]
fn a_collision_created_by_sanitisation_is_refused_in_either_order() {
    let left = catalog_of(&["c"]);
    let right = catalog_of(&["c"]);
    for connected in [
        vec![
            connected_catalog("a/b", left.as_slice()),
            connected_catalog("a_b", right.as_slice()),
        ],
        vec![
            connected_catalog("a_b", right.as_slice()),
            connected_catalog("a/b", left.as_slice()),
        ],
    ] {
        let published = tool_definitions_from(&connected, &["read", "edit", "shell"]);
        assert!(published.ambiguous.iter().all(|tool| tool.name == "a_b_c"));
        assert_eq!(published.ambiguous.len(), 2);
        assert!(published.definitions.is_empty());
    }
}

/// The refusal is NARROW: one server's collision must not suppress its
/// other, unambiguous tools, or a single misnamed server would silently
/// disarm a whole catalog.
#[test]
fn a_collision_suppresses_only_the_contested_name() {
    let ab = catalog_of(&["c", "distinct"]);
    let a = catalog_of(&["b_c"]);
    let connected: Vec<ConnectedCatalog<'_>> = vec![
        connected_catalog("a_b", ab.as_slice()),
        connected_catalog("a", a.as_slice()),
    ];

    let published = tool_definitions_from(&connected, &["read", "edit", "shell"]);
    assert!(published.ambiguous.iter().all(|tool| tool.name == "a_b_c"));
    let names: Vec<&str> = published
        .definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    assert_eq!(names, vec!["a_b_distinct"]);
}

/// The per-server bounds cap what ONE server inflates; nothing capped the
/// SUM, and the sum is what reaches a prompt. Servers each well inside
/// their own bound can together publish a catalog no single server was
/// ever allowed to.
#[test]
fn the_aggregate_catalog_is_bounded_across_all_servers_not_only_within_one() {
    // 100 each is comfortably inside the 128-tool PER-SERVER bound, so
    // every one of these servers is individually well behaved. Three of
    // them together are not, which is the whole point.
    let names: Vec<String> = (0..100).map(|index| format!("tool{index}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let first = catalog_of(&refs);
    let second = catalog_of(&refs);
    let third = catalog_of(&refs);
    let connected: Vec<ConnectedCatalog<'_>> = vec![
        connected_catalog("alpha", first.as_slice()),
        connected_catalog("beta", second.as_slice()),
        connected_catalog("gamma", third.as_slice()),
    ];

    let published = tool_definitions_from(&connected, &["read", "edit", "shell"]);
    assert_eq!(
        published.definitions.len(),
        MAX_PUBLISHED_TOOLS,
        "300 individually admissible tools stop at the aggregate bound"
    );
    assert_eq!(
        published.unpublished.len(),
        300 - MAX_PUBLISHED_TOOLS,
        "everything refused for room is RECORDED, not silently missing"
    );
    assert!(
        published.ambiguous.is_empty() && published.shadowed.is_empty(),
        "a bound is not a collision and not a shadow"
    );
    let carried: usize = published
        .definitions
        .iter()
        .map(|definition| definition.input_schema.len())
        .sum();
    assert!(carried <= MAX_PUBLISHED_SCHEMA_BYTES);
}

/// The BYTE guard, isolated from the count guard.
///
/// The count witness above would still pass if the byte bound vanished, so
/// it proves nothing about it. Here every tool is individually admissible
/// and the COUNT stays far under its cap; only the accumulated schema
/// crosses. If the byte guard disappeared, all of these would publish.
#[test]
fn the_aggregate_schema_byte_bound_is_enforced_under_the_tool_count_cap() {
    let filler = "x".repeat(20 * 1024);
    let rows: Vec<Value> = (0..40)
        .map(|index| {
            json!({"name": format!("tool{index}"), "description": "fixture",
                   "inputSchema": {"type":"object","properties":{},"title": filler}})
        })
        .collect();
    let catalog = match accept_tools(bounds(), &json!({"tools": rows})) {
        Ok(catalog) => catalog,
        Err(error) => panic!("each schema must be individually admissible: {error}"),
    };
    let connected: Vec<ConnectedCatalog<'_>> = vec![connected_catalog("bulky", catalog.as_slice())];

    let published = tool_definitions_from(&connected, &["read", "edit", "shell"]);
    assert!(
        published.definitions.len() < 40,
        "the byte bound must cut before the catalog is exhausted"
    );
    assert!(
        published.definitions.len() < MAX_PUBLISHED_TOOLS,
        "the COUNT cap is nowhere near reached, so this isolates the byte guard"
    );
    assert!(!published.unpublished.is_empty());
    let carried: usize = published
        .definitions
        .iter()
        .map(|definition| definition.input_schema.len())
        .sum();
    assert!(carried <= MAX_PUBLISHED_SCHEMA_BYTES);

    // The final resume description, not the producer's shorter unbound advice,
    // must be charged before deciding whether the tool fits.
    let schema = json!({"type":"object","title":"x".repeat(16*1024)});
    let mut rows: Vec<Value> = (0..31)
        .map(|index| {
            json!({
                "name":format!("prefix{index}"), "inputSchema":schema
            })
        })
        .collect();
    let prefix_bytes = serde_json::to_string(&schema)
        .unwrap_or_else(|error| panic!("schema: {error}"))
        .len()
        * rows.len();
    let advice = "Unbound callers name project_id and agent_id.";
    let mut resume_schema = json!({"type":"object","title":""});
    let overhead = serde_json::to_string(&resume_schema)
        .unwrap_or_else(|error| panic!("resume schema: {error}"))
        .len();
    resume_schema["title"] = "x"
        .repeat(MAX_PUBLISHED_SCHEMA_BYTES - prefix_bytes - overhead - advice.len())
        .into();
    rows.push(json!({"name":"stateengine_resume","description":advice,
        "inputSchema":resume_schema}));
    rows.push(json!({"name":"tail","inputSchema":{"type":"object"}}));
    let catalog = accept_tools(bounds(), &json!({"tools":rows}))
        .unwrap_or_else(|error| panic!("bound catalog: {error}"));
    let connected = [connected_catalog("stateengine", catalog.as_slice())];
    let unbound = tool_definitions_from(&connected, &[]);
    assert!(unbound
        .routes
        .contains_key("stateengine_stateengine_resume"));
    let bound = super::super::publication::tool_definitions_from_bound(&connected, &[], true);
    assert!(!bound.routes.contains_key("stateengine_stateengine_resume"));
    assert!(!bound.routes.contains_key("stateengine_tail"));
    let carried: usize = bound
        .definitions
        .iter()
        .map(|tool| tool.input_schema.len() + tool.description.as_ref().map_or(0, String::len))
        .sum();
    assert!(carried <= MAX_PUBLISHED_SCHEMA_BYTES);
}

/// The chosen policy, asserted rather than described: once a bound is
/// crossed nothing further publishes, even a tool that would have fit.
///
/// This is what stops the published catalog depending on tool SIZES rather
/// than order, which an operator cannot predict or explain.
#[test]
fn crossing_a_bound_stops_publication_rather_than_packing_smaller_tools_after_it() {
    let filler = "x".repeat(20 * 1024);
    let mut rows: Vec<Value> = (0..30)
        .map(|index| {
            json!({"name": format!("big{index}"), "description": "fixture",
                   "inputSchema": {"type":"object","properties":{},"title": filler}})
        })
        .collect();
    // Deliberately tiny and deliberately LAST: it would fit in the
    // remaining room under a pack-what-fits policy.
    rows.push(json!({"name":"tiny","description":"fixture",
                     "inputSchema":{"type":"object","properties":{}}}));
    let catalog = match accept_tools(bounds(), &json!({"tools": rows})) {
        Ok(catalog) => catalog,
        Err(error) => panic!("each schema must be individually admissible: {error}"),
    };
    let connected: Vec<ConnectedCatalog<'_>> = vec![connected_catalog("bulky", catalog.as_slice())];

    let published = tool_definitions_from(&connected, &["read", "edit", "shell"]);
    assert!(
        published
            .definitions
            .iter()
            .all(|definition| definition.name != "bulky_tiny"),
        "a small tool AFTER the crossing must not slip in; publication stopped"
    );
    assert!(published
        .unpublished
        .iter()
        .any(|tool| tool.name == "bulky_tiny"));
}

#[test]
fn bound_resume_guidance_replaces_unbound_advice_on_both_tool_surfaces() {
    let catalog = match accept_tools(
        bounds(),
        &json!({"tools":[{
            "name":"stateengine_resume",
            "description":"Unbound callers name project_id and agent_id.",
            "inputSchema":{"type":"object","properties":{"agent_id":{"type":"string"}}}
        }]}),
    ) {
        Ok(catalog) => catalog,
        Err(error) => panic!("catalog: {error}"),
    };
    let mut published =
        tool_definitions_from(&[connected_catalog("stateengine", catalog.as_slice())], &[]);
    let name = "stateengine_stateengine_resume";
    let original = published.routes[name].clone();
    let schema = published.definitions[0].input_schema.clone();
    assert!(published.definitions[0]
        .description
        .as_deref()
        .is_some_and(|text| text.contains("Unbound callers")));
    published = super::super::publication::tool_definitions_from_bound(
        &[connected_catalog("stateengine", catalog.as_slice())],
        &[],
        true,
    );
    let expected = super::super::super::stateengine_resume::BOUND_RESUME_GUIDANCE;
    assert_eq!(
        published.definitions[0].description.as_deref(),
        Some(expected)
    );
    assert_eq!(
        published.routes[name].description.as_deref(),
        Some(expected)
    );
    assert_eq!(published.definitions[0].input_schema, schema);
    let mut route = published.routes[name].clone();
    route.description = original.description.clone();
    assert_eq!(route, original);
}

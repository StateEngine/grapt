//! Witnesses for the protocol domain.
//!
//! Beside their subject rather than in a `tests/` tree: they drive the exact
//! refusals this crate exists to make, several of which are only observable
//! through its private encoding helpers.

use super::*;

type TestResult<T = ()> = Result<T, McpProtocolError>;

#[test]
fn paginated_catalog_requires_completion_and_preserves_second_page() -> TestResult {
    let mut catalog = discovery::ToolCatalog::new(bounds());
    assert!(!catalog.accept_page(&json!({"tools":[tool("first")],"nextCursor":"page-2"}))?);
    assert_eq!(catalog.cursor(), Some("page-2"));
    assert!(catalog.accept_page(&json!({"tools":[tool("second")]}))?);
    let tools = catalog.finish()?;
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[1].name().as_str(), "second");
    let mut incomplete = discovery::ToolCatalog::new(bounds());
    incomplete.accept_page(&json!({"tools":[],"nextCursor":"next"}))?;
    assert!(matches!(
        incomplete.finish(),
        Err(McpProtocolError::ToolCatalogIncomplete)
    ));
    Ok(())
}

#[test]
fn paginated_catalog_refuses_cycles_malformed_cursors_and_duplicate_names() -> TestResult {
    for cursor in [json!(null), json!(3), json!(""), json!("x".repeat(1025))] {
        let mut catalog = discovery::ToolCatalog::new(bounds());
        assert!(matches!(
            catalog.accept_page(&json!({"tools":[],"nextCursor":cursor})),
            Err(McpProtocolError::InvalidToolsCursor)
        ));
    }
    let mut cycle = discovery::ToolCatalog::new(bounds());
    cycle.accept_page(&json!({"tools":[],"nextCursor":"a"}))?;
    cycle.accept_page(&json!({"tools":[],"nextCursor":"b"}))?;
    assert!(matches!(
        cycle.accept_page(&json!({"tools":[],"nextCursor":"a"})),
        Err(McpProtocolError::RepeatedToolsCursor)
    ));
    let mut duplicate = discovery::ToolCatalog::new(bounds());
    duplicate.accept_page(&json!({"tools":[tool("same")],"nextCursor":"next"}))?;
    assert!(matches!(
        duplicate.accept_page(&json!({"tools":[tool("same")]})),
        Err(McpProtocolError::ToolRepeated(_))
    ));
    Ok(())
}

#[test]
fn paginated_catalog_bounds_aggregate_tools_bytes_and_empty_page_requests(
) -> Result<(), Box<dyn std::error::Error>> {
    let limits = McpBounds::new(4096, 2, 512, 64, 128)?;
    let mut tools = discovery::ToolCatalog::new(limits);
    tools.accept_page(&json!({"tools":[tool("one")],"nextCursor":"next"}))?;
    assert!(matches!(
        tools.accept_page(&json!({"tools":[tool("two"),tool("three")]})),
        Err(McpProtocolError::TooManyTools { .. })
    ));
    let mut pages = discovery::ToolCatalog::new(limits);
    pages.accept_page(&json!({"tools":[],"nextCursor":"one"}))?;
    assert!(matches!(
        pages.accept_page(&json!({"tools":[],"nextCursor":"two"})),
        Err(McpProtocolError::TooManyToolPages { .. })
    ));
    let mut bytes = discovery::ToolCatalog::new(McpBounds::new(100, 4, 64, 16, 16)?);
    bytes.accept_page(&json!({"tools":[],"nextCursor":"one","extra":"x".repeat(30)}))?;
    assert!(matches!(
        bytes.accept_page(&json!({"tools":[],"extra":"x".repeat(30)})),
        Err(McpProtocolError::ToolCatalogTooLarge { .. })
    ));
    Ok(())
}

fn bounds() -> McpBounds {
    match McpBounds::conservative() {
        Ok(bounds) => bounds,
        Err(_) => panic!("the conservative bounds must be constructible"),
    }
}

fn tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": "reads a fixture record",
        "inputSchema": {"type":"object","properties":{"id":{"type":"string"}}},
    })
}

/// The revision this client offers must be one it also understands.
/// Offering something outside the accepted set would refuse a server that
/// simply agreed with us.
#[test]
fn the_offered_revision_is_one_this_client_accepts() {
    assert!(SUPPORTED_PROTOCOL_VERSIONS.contains(&PROTOCOL_VERSION));
}

#[test]
fn every_understood_revision_negotiates_and_is_recorded_as_the_servers_answer() -> TestResult {
    for offered in SUPPORTED_PROTOCOL_VERSIONS {
        let identity = accept_initialize(&json!({
            "protocolVersion": offered,
            "serverInfo": {"name": "fixture-server"},
        }))?;
        assert_eq!(
            identity.negotiated_protocol(),
            *offered,
            "the settled revision is what the server answered"
        );
    }
    Ok(())
}

#[test]
fn the_initialized_notification_carries_no_id_and_no_parameters() -> TestResult {
    let notification = initialized_notification(bounds())?;
    let sent: Value = match serde_json::from_slice(notification.frame()) {
        Ok(value) => value,
        Err(error) => panic!("the encoded frame must be JSON: {error}"),
    };
    assert_eq!(sent["jsonrpc"], "2.0");
    assert_eq!(sent["method"], "notifications/initialized");
    // A notification with an id is a request, and a server that treats it
    // as one owes a reply nobody is waiting for.
    assert!(sent.get("id").is_none());
    assert!(sent.get("params").is_none());
    Ok(())
}

fn tool_name(raw: &str) -> McpToolName {
    match McpToolName::new(raw) {
        Some(name) => name,
        None => panic!("{raw:?} must be a usable tool name"),
    }
}

#[test]
fn a_call_names_the_discovered_tool_and_passes_its_arguments_through() -> TestResult {
    let mut sequence = RequestSequence::new();
    let request = tools_call_request(
        &mut sequence,
        bounds(),
        &tool_name("fixture_read"),
        json!({"id": "42"}),
    )?;
    let sent: Value = match serde_json::from_slice(request.frame()) {
        Ok(value) => value,
        Err(error) => panic!("the encoded frame must be JSON: {error}"),
    };
    assert_eq!(sent["method"], "tools/call");
    assert_eq!(sent["params"]["name"], "fixture_read");
    assert_eq!(
        sent["params"]["arguments"],
        json!({"id":"42"}),
        "arguments reach the server as the model produced them"
    );
    Ok(())
}

#[test]
fn arguments_that_are_not_an_object_are_refused_before_they_reach_a_server() {
    let mut sequence = RequestSequence::new();
    assert!(matches!(
        tools_call_request(
            &mut sequence,
            bounds(),
            &tool_name("fixture_read"),
            json!([1, 2])
        ),
        Err(McpProtocolError::CallArgumentsNotAnObject(_))
    ));
}

/// A tool that ran and failed is not a call that could not be made. The
/// server's own verdict is carried, and a result with no `isError` claims
/// no error rather than leaving the question open.
#[test]
fn a_tools_own_failure_verdict_is_carried_rather_than_translated() -> TestResult {
    let failed = accept_call(
        bounds(),
        &json!({"content":[{"type":"text","text":"exit 1"}],"isError":true}),
    )?;
    assert!(failed.is_error());
    assert_eq!(failed.text(), "exit 1");

    let succeeded = accept_call(bounds(), &json!({"content":[{"type":"text","text":"ok"}]}))?;
    assert!(
        !succeeded.is_error(),
        "an absent isError is the specification's no-error default"
    );
    assert_eq!(succeeded.structured(), None, "absent is not empty");
    Ok(())
}

#[test]
fn an_empty_result_is_a_real_answer_and_a_broken_one_is_a_refusal() -> TestResult {
    let empty = accept_call(bounds(), &json!({"content":[]}))?;
    assert_eq!(
        empty.text(),
        "",
        "a tool that returned nothing returned nothing"
    );
    assert!(!empty.is_error());

    assert!(matches!(
        accept_call(bounds(), &json!({})),
        Err(McpProtocolError::CallMissingContent)
    ));
    assert!(matches!(
        accept_call(bounds(), &json!({"content":"text"})),
        Err(McpProtocolError::CallContentNotAnArray)
    ));
    assert!(matches!(
        accept_call(bounds(), &json!({"content":[{"text":"no type"}]})),
        Err(McpProtocolError::CallBlockUntyped)
    ));
    assert!(matches!(
        accept_call(
            bounds(),
            &json!({"content":[{"type":"text","text":"x"}],"isError":"true"})
        ),
        Err(McpProtocolError::CallErrorFlagNotABool),
    ));
    Ok(())
}

/// A block this client cannot render is named, never dropped. A dropped
/// block would leave the model continuing from an absence that was in fact
/// a value it was never shown.
#[test]
fn a_content_block_this_client_cannot_render_is_named_not_discarded() -> TestResult {
    let outcome = accept_call(
        bounds(),
        &json!({"content":[
            {"type":"text","text":"before"},
            {"type":"image","data":"...","mimeType":"image/png"},
            {"type":"text","text":"after"}]}),
    )?;
    assert_eq!(
        outcome.text(),
        "before\n[unsupported MCP content block: image]\nafter"
    );
    Ok(())
}

#[test]
fn a_tool_result_above_the_declared_bound_is_refused_rather_than_absorbed() {
    let huge = "x".repeat(bounds().max_schema_bytes() + 1);
    assert!(matches!(
        accept_call(bounds(), &json!({"content":[{"type":"text","text":huge}]})),
        Err(McpProtocolError::CallResultTooLarge { .. })
    ));
}

#[test]
fn structured_and_binary_content_survive_without_changing_direct_text() -> TestResult {
    let content = json!([
        {"type":"text","text":"preview"},
        {"type":"resource","resource":{"uri":"resource://text","text":"embedded"}},
        {"type":"resource_link","uri":"resource://docs","name":"docs","title":"Documentation"},
        {"type":"resource_link","uri":"resource://more","name":"More"},
        {"type":"image","data":"AQID","mimeType":"image/png"},
        {"type":"resource","resource":{"uri":"resource://binary","blob":"AQID"}},
        {"type":"future"},
        {"type":"text","text":"after"}
    ]);
    let outcome = accept_call(
        bounds(),
        &json!({"content":content,"structuredContent":{"rows":[1,2]},"isError":true}),
    )?;
    assert!(outcome.is_error());
    assert_eq!(outcome.structured(), Some(&json!({"rows":[1,2]})));
    assert_eq!(
        outcome.content(),
        content
            .as_array()
            .unwrap_or_else(|| panic!("fixture content"))
    );
    assert_eq!(
        outcome.text(),
        concat!(
            "preview\nembedded\nDocumentation: resource://docs\nMore: resource://more\n",
            "[unsupported MCP content block: image]\n",
            "[unsupported MCP content block: resource]\n",
            "[unsupported MCP content block: future]\nafter"
        )
    );
    let narrow = McpBounds::new(1024, 8, 64, 128, 128)
        .unwrap_or_else(|error| panic!("narrow bounds: {error}"));
    for block in [
        json!({"type":"resource","resource":{"uri":"resource://long","text":"x".repeat(65)}}),
        json!({"type":"resource_link","uri":"x".repeat(65),"name":"Long"}),
    ] {
        assert!(matches!(
            accept_call(narrow, &json!({"content":[block]})),
            Err(McpProtocolError::CallResultTooLarge { limit: 64, .. })
        ));
    }
    Ok(())
}

#[test]
fn non_text_payloads_and_join_separators_cannot_bypass_result_bounds() -> TestResult {
    for value in [
        json!({"content":[],"structuredContent":{"huge":"x".repeat(bounds().max_frame_bytes())}}),
        json!({"content":[{"type":"image","data":"x".repeat(bounds().max_frame_bytes()),"mimeType":"image/png"}]}),
    ] {
        assert!(matches!(
            accept_call(bounds(), &value),
            Err(McpProtocolError::CallResultTooLarge { .. })
        ));
    }
    let narrow = McpBounds::new(1024, 8, 3, 128, 128)
        .unwrap_or_else(|error| panic!("narrow bounds: {error}"));
    assert!(matches!(
        accept_call(
            narrow,
            &json!({"content":[{"type":"text","text":"aa"},{"type":"text","text":"b"}]})
        ),
        Err(McpProtocolError::CallResultTooLarge { limit: 3, .. })
    ));
    Ok(())
}

#[test]
fn a_handshake_and_a_discovery_round_trip_within_their_bounds() -> TestResult {
    let mut sequence = RequestSequence::new();
    let initialize = initialize_request(&mut sequence, bounds(), "grapt", "0.1.0")?;
    let sent: Value = match serde_json::from_slice(initialize.frame()) {
        Ok(value) => value,
        Err(error) => panic!("the encoded frame must be JSON: {error}"),
    };
    assert_eq!(sent["jsonrpc"], "2.0");
    assert_eq!(sent["method"], "initialize");
    assert_eq!(sent["params"]["protocolVersion"], PROTOCOL_VERSION);
    assert!(sent["params"]["capabilities"]["tools"].is_object());
    for mode in ["form", "url"] {
        assert!(
            sent["params"]["capabilities"]["elicitation"][mode].is_object(),
            "the client must advertise supported {mode} elicitation"
        );
    }
    assert_eq!(
        sent["params"]["capabilities"]["roots"]["listChanged"].as_bool(),
        Some(true)
    );
    assert_eq!(sent["id"], 1);

    let reply = json!({
        "jsonrpc":"2.0","id":1,
        "result":{"protocolVersion":PROTOCOL_VERSION,
                  "instructions":"Use fixture_read for exact record identifiers.",
                  "serverInfo":{"name":"fixture-server","version":"9.9"}},
    })
    .to_string();
    let result = decode_reply(bounds(), initialize.id(), reply.as_bytes())?;
    let identity = accept_initialize(&result)?;
    assert_eq!(identity.name(), "fixture-server");
    assert_eq!(identity.version(), Some("9.9"));
    assert_eq!(
        identity.instructions(),
        Some("Use fixture_read for exact record identifiers.")
    );
    assert!(
        !identity.advertises_resources(),
        "a server that claimed no resources capability must not be read as offering one"
    );
    let with_resources = accept_initialize(&json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"resources": {}},
        "serverInfo": {"name": "fixture-server"},
    }))?;
    assert!(with_resources.advertises_resources());

    let listing = tools_list_request(&mut sequence, bounds())?;
    assert_eq!(listing.id().value(), 2, "ids are issued in order");
    let reply =
        json!({"jsonrpc":"2.0","id":2,"result":{"tools":[tool("fixture_read")]}}).to_string();
    let result = decode_reply(bounds(), listing.id(), reply.as_bytes())?;
    let tools = accept_tools(bounds(), &result)?;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name().as_str(), "fixture_read");
    assert_eq!(tools[0].description(), Some("reads a fixture record"));
    assert_eq!(tools[0].input_schema()["type"], "object");
    Ok(())
}

#[test]
fn a_server_with_no_tools_is_not_a_server_whose_listing_failed() -> TestResult {
    let empty = accept_tools(bounds(), &json!({"tools":[]}))?;
    assert!(empty.is_empty(), "no tools is a real answer");
    assert!(
        matches!(
            accept_tools(bounds(), &json!({})),
            Err(McpProtocolError::ToolsNotAnArray)
        ),
        "an unreadable listing must not collapse into an empty catalog"
    );
    assert!(matches!(
        accept_tools(bounds(), &json!({"tools":{}})),
        Err(McpProtocolError::ToolsNotAnArray)
    ));
    Ok(())
}

#[test]
fn output_schema_is_retained_and_malformed_is_not_absent() -> TestResult {
    let mut row = tool("fixture_read");
    let absent = accept_tools(bounds(), &json!({"tools":[row.clone()]}))?;
    assert!(absent[0].output_schema().is_none());
    let schema = json!({"type":"object","required":["sum"]});
    row["outputSchema"] = schema.clone();
    let present = accept_tools(bounds(), &json!({"tools":[row.clone()]}))?;
    assert_eq!(present[0].output_schema(), Some(&schema));
    for invalid in [Value::Null, json!("object"), json!([])] {
        row["outputSchema"] = invalid;
        assert!(matches!(
            accept_tools(bounds(), &json!({"tools":[row.clone()]})),
            Err(McpProtocolError::SchemaNotAnObject(_))
        ));
    }
    row["outputSchema"] = json!({"description":"x".repeat(bounds().max_schema_bytes)});
    assert!(matches!(
        accept_tools(bounds(), &json!({"tools":[row]})),
        Err(McpProtocolError::SchemaTooLarge { .. })
    ));
    Ok(())
}

#[test]
fn a_late_or_forged_reply_cannot_answer_a_different_request() -> TestResult {
    let mut sequence = RequestSequence::new();
    let first = initialize_request(&mut sequence, bounds(), "grapt", "0.1.0")?;
    let second = tools_list_request(&mut sequence, bounds())?;
    let stale = json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}}).to_string();
    assert!(
        matches!(
            decode_reply(bounds(), second.id(), stale.as_bytes()),
            Err(McpProtocolError::CorrelationMismatch {
                expected: 2,
                replied: 1
            })
        ),
        "a reply to the retired request was delivered into the live one"
    );
    // The correct correlation still works, so the check is not vacuous.
    let answered = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}}).to_string();
    assert!(decode_reply(bounds(), first.id(), answered.as_bytes()).is_ok());
    Ok(())
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_hostile_or_confused_server_is_refused_at_every_declared_bound() -> TestResult {
    let mut sequence = RequestSequence::new();
    let request = tools_list_request(&mut sequence, bounds())?;
    let id = request.id();

    // Framing and envelope.
    let oversized = vec![b'x'; bounds().max_frame_bytes() + 1];
    assert!(matches!(
        decode_reply(bounds(), id, &oversized),
        Err(McpProtocolError::FrameTooLarge { .. })
    ));
    assert!(matches!(
        decode_reply(bounds(), id, b"not json"),
        Err(McpProtocolError::Malformed(_))
    ));
    assert!(matches!(
        decode_reply(bounds(), id, b"[]"),
        Err(McpProtocolError::NotAnObject)
    ));
    assert!(matches!(
        decode_reply(bounds(), id, br#"{"jsonrpc":"1.0","id":1,"result":{}}"#),
        Err(McpProtocolError::WrongVersion(_))
    ));
    assert!(matches!(
        decode_reply(bounds(), id, br#"{"jsonrpc":"2.0","result":{}}"#),
        Err(McpProtocolError::UnusableId)
    ));
    assert!(matches!(
        decode_reply(bounds(), id, br#"{"jsonrpc":"2.0","id":1}"#),
        Err(McpProtocolError::NeitherResultNorError)
    ));
    assert!(matches!(
        decode_reply(
            bounds(),
            id,
            br#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":1,"message":"x"}}"#
        ),
        Err(McpProtocolError::BothResultAndError)
    ));
    assert!(matches!(
        decode_reply(
            bounds(),
            id,
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"nope"}}"#
        ),
        Err(McpProtocolError::ServerError { code: -32000, .. })
    ));

    // Handshake.
    for instructions in [json!(null), json!(4), json!("x".repeat(65_537))] {
        assert!(matches!(
            accept_initialize(
                &json!({"protocolVersion":PROTOCOL_VERSION,"serverInfo":{"name":"fixture"},"instructions":instructions})
            ),
            Err(McpProtocolError::InvalidServerInstructions)
        ));
    }
    assert!(matches!(
        accept_initialize(&json!({"protocolVersion":"1999-01-01","serverInfo":{"name":"x"}})),
        Err(McpProtocolError::ProtocolVersionUnsupported { .. })
    ));
    assert!(matches!(
        accept_initialize(&json!({"protocolVersion":PROTOCOL_VERSION})),
        Err(McpProtocolError::MissingServerIdentity)
    ));
    assert!(
        matches!(
            accept_initialize(
                &json!({"protocolVersion":PROTOCOL_VERSION,"serverInfo":{"name":""}})
            ),
            Err(McpProtocolError::MissingServerIdentity)
        ),
        "an empty name is not an identity"
    );

    // Catalog.
    let many: Vec<Value> = (0..bounds().max_tools() + 1)
        .map(|index| tool(&format!("tool_{index}")))
        .collect();
    assert!(matches!(
        accept_tools(bounds(), &json!({"tools":many})),
        Err(McpProtocolError::TooManyTools { .. })
    ));
    let huge = "y".repeat(bounds().max_schema_bytes());
    assert!(matches!(
        accept_tools(
            bounds(),
            &json!({"tools":[{"name":"big","inputSchema":{"type":"object","note":huge}}]})
        ),
        Err(McpProtocolError::SchemaTooLarge { .. })
    ));
    assert!(matches!(
        accept_tools(bounds(), &json!({"tools":[{"name":"no_schema"}]})),
        Err(McpProtocolError::ToolMissing("inputSchema"))
    ));
    assert!(matches!(
        accept_tools(
            bounds(),
            &json!({"tools":[{"name":"bad_schema","inputSchema":[]}]})
        ),
        Err(McpProtocolError::SchemaNotAnObject(_))
    ));
    assert!(matches!(
        accept_tools(
            bounds(),
            &json!({"tools":[{"inputSchema":{"type":"object"}}]})
        ),
        Err(McpProtocolError::ToolMissing("name"))
    ));
    assert!(
        matches!(
            accept_tools(bounds(), &json!({"tools":[tool("same"), tool("same")]})),
            Err(McpProtocolError::ToolRepeated(_))
        ),
        "a repeated tool name would make one advertised tool unreachable"
    );
    Ok(())
}

#[test]
fn bounds_and_ids_are_themselves_bounded() {
    assert!(McpBounds::new(0, 1, 1, 1, 1).is_err());
    assert!(McpBounds::new(1, 0, 1, 1, 1).is_err());
    assert!(McpBounds::new(1, 1, 0, 1, 1).is_err());
    assert!(McpBounds::new(1, 1, 1, 0, 1).is_err());
    assert!(McpBounds::new(1, 1, 1, 1, 0).is_err());
    let mut exhausted = RequestSequence(u64::MAX);
    assert!(matches!(
        exhausted.mint(),
        Err(McpProtocolError::RequestIdExhausted)
    ));
}

#[test]
fn scc_private_metadata_has_its_own_slot_and_never_formats_its_bytes() -> TestResult {
    let name = tool_name("resume");
    let arguments = json!({"_meta":{"binding":"model-forgery"},"since":"cursor"});
    let plain = tools_call_request(
        &mut RequestSequence::new(),
        bounds(),
        &name,
        arguments.clone(),
    )?;
    let unchanged = tools_call_request_with_metadata(
        &mut RequestSequence::new(),
        bounds(),
        &name,
        arguments.clone(),
        None,
    )?;
    assert_eq!(plain.frame(), unchanged.frame());
    let metadata = json!({"binding":"scc-secret-capability"});
    let private = tools_call_request_with_metadata(
        &mut RequestSequence::new(),
        bounds(),
        &name,
        arguments.clone(),
        Some(metadata.clone()),
    )?;
    let value: Value = serde_json::from_slice(private.frame())
        .map_err(|error| McpProtocolError::Malformed(error.to_string()))?;
    assert_eq!(
        value,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"resume","arguments":arguments,"_meta":metadata}})
    );
    assert_eq!(
        format!("{private:?}"),
        "OutboundRequest { id: RequestId(1), .. }"
    );
    assert!(!format!("{private:?}").contains("scc-secret"));
    for metadata in [Value::Null, json!([]), json!("scc-secret-capability")] {
        let failure = tools_call_request_with_metadata(
            &mut RequestSequence::new(),
            bounds(),
            &name,
            json!({}),
            Some(metadata),
        );
        assert!(matches!(
            failure,
            Err(McpProtocolError::CallMetadataNotAnObject)
        ));
    }
    let secret = format!(
        "scc-secret-capability{}",
        "x".repeat(bounds().max_frame_bytes)
    );
    let failure = tools_call_request_with_metadata(
        &mut RequestSequence::new(),
        bounds(),
        &name,
        json!({}),
        Some(json!({"binding":secret})),
    );
    assert!(matches!(
        failure,
        Err(McpProtocolError::PrivateCallFrameTooLarge)
    ));
    assert_eq!(format!("{failure:?}"), "Err(PrivateCallFrameTooLarge)");
    Ok(())
}

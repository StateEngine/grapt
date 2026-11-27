use std::io::Cursor;

use grapt_codemode::{
    bounded_json, read_frame, write_frame, CatalogTool, ExecuteRequest, HostReply, ProtocolError,
    FRAME_BYTES,
};
use grapt_types::{ToolDefinitionId, ToolSchemaVersion};
use serde_json::json;

fn request() -> ExecuteRequest {
    ExecuteRequest {
        code: String::new(),
        catalog: ToolSchemaVersion::new("catalog").unwrap_or_else(|| panic!("catalog")),
        tools: vec![CatalogTool {
            name: ToolDefinitionId::new("echo").unwrap_or_else(|| panic!("name")),
            description: "Echo".into(),
            input_schema: json!({"type":"object"}),
            output_schema: Some(json!({"type":"object"})),
        }],
    }
}

#[test]
fn frames_refuse_oversized_prefix_before_reading_body_and_reject_extra_fields() {
    for length in [0, FRAME_BYTES + 1] {
        let length = u32::try_from(length).unwrap_or_else(|_| panic!("frame length"));
        let mut input = Cursor::new(length.to_le_bytes());
        assert!(matches!(
            read_frame::<_, HostReply>(&mut input),
            Err(ProtocolError::Invalid("frame"))
        ));
        assert_eq!(input.position(), 4);
    }
    let mut input = Vec::new();
    write_frame(
        &mut input,
        &json!({"ordinal":0,"result":{"status":"completed","value":null},"extra":true}),
    )
    .unwrap_or_else(|error| panic!("fixture frame: {error}"));
    assert!(matches!(
        read_frame::<_, HostReply>(&mut Cursor::new(input)),
        Err(ProtocolError::Json(_))
    ));
    assert!(bounded_json(&json!("x".repeat(FRAME_BYTES)), FRAME_BYTES).is_err());
}

#[test]
fn catalog_and_source_bounds_preserve_utf8_and_refuse_ambiguous_shapes() {
    let mut offered = request();
    offered.code = "é".repeat(32 * 1024);
    assert!(offered.validate().is_ok());
    offered.code.push('é');
    assert!(matches!(
        offered.validate(),
        Err(ProtocolError::Invalid("source"))
    ));
    offered = request();
    offered.tools[0].output_schema = Some(json!(false));
    assert!(offered.validate().is_err());
    offered = request();
    offered.tools[0].description = "x".repeat(4097);
    assert!(offered.validate().is_err());
    offered = request();
    offered.tools.push(offered.tools[0].clone());
    assert!(offered.validate().is_err());
    offered = request();
    offered.tools = (0..=256)
        .map(|index| {
            let mut tool = offered.tools[0].clone();
            tool.name = ToolDefinitionId::new(format!("tool{index}"))
                .unwrap_or_else(|| panic!("bounded tool name"));
            tool
        })
        .collect();
    assert!(offered.validate().is_err());
}

#[test]
fn catalog_transport_remains_bounded_after_the_publication_capacity_increase() {
    let mut offered = request();
    offered.tools[0].input_schema["description"] = json!("x".repeat(grapt_codemode::CATALOG_BYTES));
    assert!(offered.validate().is_err());
}

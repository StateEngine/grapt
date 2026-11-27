//! What a request may carry, judged per definition rather than per request.
use grapt_provider::tool_eligibility::{eligibility, Eligibility, ExclusionReason};
use grapt_provider::{ToolDefinition, ToolInputFormat};

fn tool(schema: &str) -> ToolDefinition {
    ToolDefinition {
        input_format: ToolInputFormat::Json,
        name: "read".into(),
        description: None,
        input_schema: schema.into(),
    }
}

#[test]
fn a_malformed_schema_is_excluded_with_its_reason_rather_than_sent() {
    // An MCP server's own schema can be malformed. Excluding the whole tool
    // lets the turn proceed; sending it would be a malformed request, and
    // refusing the turn would fail every later turn at the same revision.
    for (schema, reason) in [
        ("[", ExclusionReason::SchemaNotJson),
        ("{", ExclusionReason::SchemaNotJson),
        ("[]", ExclusionReason::SchemaNotObject),
        ("3", ExclusionReason::SchemaNotObject),
    ] {
        assert_eq!(
            eligibility(&tool(schema)),
            Eligibility::Excluded(reason),
            "schema {schema:?}"
        );
    }
    assert_eq!(eligibility(&tool("{}")), Eligibility::Eligible);
}

#[test]
fn no_reason_variant_ships_wordless() {
    for reason in [
        ExclusionReason::DescriptionTooLarge,
        ExclusionReason::GrammarTooLarge,
        ExclusionReason::DescriptionEmpty,
        ExclusionReason::GrammarEmpty,
        ExclusionReason::SchemaNotJson,
        ExclusionReason::SchemaNotObject,
    ] {
        assert!(!reason.as_str().is_empty(), "reason {reason:?}");
    }
}

#[test]
fn descriptions_up_to_4k_are_admitted_and_oversized_descriptions_report_reason() {
    use grapt_provider::tool_eligibility::MAX_TOOL_DESCRIPTION_BYTES;
    let mut medium = tool("{}");
    medium.description = Some("d".repeat(1024));
    assert_eq!(eligibility(&medium), Eligibility::Eligible);

    let mut at_limit = tool("{}");
    at_limit.description = Some("d".repeat(MAX_TOOL_DESCRIPTION_BYTES));
    assert_eq!(eligibility(&at_limit), Eligibility::Eligible);

    let mut over_limit = tool("{}");
    over_limit.description = Some("d".repeat(MAX_TOOL_DESCRIPTION_BYTES + 1));
    assert_eq!(
        eligibility(&over_limit),
        Eligibility::Excluded(ExclusionReason::DescriptionTooLarge)
    );
}

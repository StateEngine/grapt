use std::io::Cursor;

use grapt_codemode::{
    read_frame, run_worker, write_frame, CatalogTool, ExecuteRequest, GuestResult, WorkerMessage,
};
use grapt_types::{ToolDefinitionId, ToolSchemaVersion};
use serde_json::{json, Value};

fn tool(name: &str, description: &str, input_schema: Value) -> Result<CatalogTool, String> {
    Ok(CatalogTool {
        name: ToolDefinitionId::new(name).ok_or("tool identity")?,
        description: description.into(),
        input_schema,
        output_schema: None,
    })
}

fn search(
    tools: Vec<CatalogTool>,
    query: &str,
    limit: usize,
) -> Result<Value, Box<dyn std::error::Error>> {
    let request = ExecuteRequest {
        code: format!(
            "return catalog.search({}, {limit});",
            serde_json::to_string(query)?
        ),
        catalog: ToolSchemaVersion::new("catalog-search").ok_or("catalog identity")?,
        tools,
    };
    let mut input = Vec::new();
    write_frame(&mut input, &request)?;
    let mut output = Vec::new();
    run_worker(&mut Cursor::new(input), &mut output)?;
    let length = output.len() as u64;
    let mut output = Cursor::new(output);
    let result: WorkerMessage = read_frame(&mut output)?;
    assert_eq!(
        output.position(),
        length,
        "discovery must emit no host call"
    );
    match result {
        WorkerMessage::Finished {
            result: GuestResult::Completed { value: Some(value) },
        } => Ok(value),
        other => Err(format!("search failed: {other:?}").into()),
    }
}

fn stateengine() -> Result<Vec<CatalogTool>, String> {
    Ok(vec![
        tool("stateengine_stateengine_resume", "Use when picking up work you cannot remember.", json!({"type":"object"}))?,
        tool("stateengine_stateengine_msg_list", "Read mail titles before opening bodies with stateengine_msg_read; unread_only starts oldest-first.", json!({
            "type":"object", "properties":{"dismiss":{"type":"boolean", "description":"Bulk-dismiss your inbox; needs before, from or kind."}}
        }))?,
        tool("stateengine_stateengine_msg_send", "Use to send findings or coordinate work.", json!({"type":"object"}))?,
    ])
}

#[test]
fn literal_queries_find_deferred_mail_without_requiring_every_word(
) -> Result<(), Box<dyn std::error::Error>> {
    let inbox = search(stateengine()?, "inbox", 16)?;
    assert_eq!(inbox["total"], 1);
    assert_eq!(
        inbox["matches"][0]["name"],
        "stateengine_stateengine_msg_list"
    );
    assert!(inbox["matches"][0].get("input_schema").is_none());
    let sentence = search(
        stateengine()?,
        "stateengine resume inbox messages read send",
        8,
    )?;
    assert_eq!(sentence["total"], 3);
    for tool in stateengine()? {
        assert!(sentence["matches"]
            .as_array()
            .ok_or("matches")?
            .iter()
            .any(|row| row["name"] == tool.name.as_str()));
    }
    let exact = search(stateengine()?, "stateengine_resume", 1)?;
    assert_eq!(
        exact["matches"][0]["name"],
        "stateengine_stateengine_resume"
    );
    assert_eq!(exact["truncated"], true);
    // Skills are built-ins; do not fabricate a deferred registration.
    assert_eq!(search(stateengine()?, "skill", 8)?["total"], 0);
    Ok(())
}

#[test]
fn search_ranks_names_then_descriptions_then_schema_and_keeps_ties_stable(
) -> Result<(), Box<dyn std::error::Error>> {
    let tools = vec![
        tool(
            "schema",
            "Other",
            json!({"type":"object","properties":{"nested":{"description":"Inbox"}}}),
        )?,
        tool("description", "Inbox", json!({"type":"object"}))?,
        tool("inbox_first", "Other", json!({"type":"object"}))?,
        tool("inbox_second", "Other", json!({"type":"object"}))?,
    ];
    let result = search(tools.clone(), "INBOX!?", 3)?;
    let names: Vec<_> = result["matches"]
        .as_array()
        .ok_or("matches")?
        .iter()
        .map(|row| row["name"].clone())
        .collect();
    assert_eq!(
        names,
        vec![
            json!("inbox_first"),
            json!("inbox_second"),
            json!("description")
        ]
    );
    assert_eq!(result["total"], 4);
    assert_eq!(result["truncated"], true);
    assert_eq!(
        search(tools.clone(), "inbox inbox", 3)?,
        search(tools.clone(), "inbox", 3)?
    );
    assert_eq!(search(tools.clone(), "absent", 3)?["total"], 0);
    let all = search(tools, "", 2)?;
    assert_eq!(all["total"], 4);
    assert_eq!(all["matches"][0]["name"], "schema");
    assert_eq!(all["matches"][1]["name"], "description");
    Ok(())
}

use super::*;

#[test]
fn msys_permission_history_keeps_shell_identity_without_accepting_foreign_tools(
) -> Result<(), String> {
    for tool in [
        "exec_command",
        "shell",
        "write_stdin",
        "read",
        "mcp.exec_command",
    ] {
        for terminal in [false, true] {
            let mut fixture = Fixture::new("msys-history")?;
            fixture.record_exchange_call(
                (terminal, false, false),
                tool,
                grapt_permission::SANDBOX_MSYS_ACTION,
                b"MSYS command outcome",
                r#"{"cmd":"bash -c true","msys_unconfined":true}"#,
            )?;
            let call = ToolCallId::new("call_d2a_read").ok_or("call")?;
            let projected = tool_projection(
                fixture.inbox()?,
                fixture.artifacts()?,
                &fixture.session,
                &call,
            );
            if !matches!(tool, "exec_command" | "shell") {
                let error = projected
                    .err()
                    .ok_or("foreign MSYS tool identity accepted")?;
                assert!(error.contains("conflicts with its permission action"));
                continue;
            }
            assert_eq!(projected?.name.as_str(), tool);
            if !terminal {
                continue;
            }
            let listed: serde_json::Value = serde_json::from_slice(&history(
                fixture.inbox()?,
                fixture.artifacts()?,
                &fixture.session,
                None,
                None,
                None,
            )?)
            .map_err(|error| error.to_string())?;
            assert_eq!(listed["calls"][0]["tool"], tool);
            fixture.close();
            let input = fixture
                .reopen(fixture.input.clone())?
                .request_input("use the tool", true)?;
            assert!(input.iter().any(|item| matches!(item,
                CodexInputItem::ToolCall { tool: name, .. } if name.as_str() == tool)));
            assert!(input.iter().any(|item| matches!(item,
                CodexInputItem::ToolOutput { output, .. } if output.as_str() == "MSYS command outcome")));
        }
    }
    Ok(())
}

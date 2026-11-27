use super::*;

#[test]
fn all_mcp_servers_default_to_discovery_and_explicit_flags_win() -> Result<(), String> {
    for declaration in [
        json!({"type":"local","command":["fixture"]}),
        json!({"type":"remote","url":"https://fixture.invalid/mcp"}),
    ] {
        for flag in [None, Some(true), Some(false)] {
            let mut local = declaration.clone();
            if let Some(flag) = flag {
                local["codemode"] = json!(flag);
            }
            let snapshot = snapshot_from(
                &json!({"mcp":{"stateengine":local.clone(),"other":local}}).to_string(),
            )?;
            assert_eq!(
                snapshot.codemode_servers.contains("stateengine"),
                flag != Some(false),
                "StateEngine flag {flag:?}"
            );
            assert_eq!(
                snapshot.codemode_servers.contains("other"),
                flag != Some(false),
                "other server flag {flag:?}"
            );
        }
    }
    Ok(())
}

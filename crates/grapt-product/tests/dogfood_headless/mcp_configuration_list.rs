//! The configuration route lists declared servers without starting a peer.
use super::*;

/// Query component encoding for a path value; the brackets in
/// `location[directory]` stay literal, the path itself is escaped.
pub(super) fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[test]
fn mcp_configuration_list_reports_declared_servers_and_starts_no_peer(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("mcp-configuration-list");
    prepare(&root, 120_000);
    let config = root.join("global-config");
    fs::create_dir_all(&config)?;
    let marker = root.join("peer-started.marker");
    // If any peer of a declared server were started, its command leaves the
    // marker; the configuration route must never reach that far.
    let user_config = serde_json::json!({
        "mcp": {"servers": {
            "declared": {
                "type": "local",
                "command": ["sh", "-c", format!("touch {}; sleep 30", marker.display())],
                "environment": {"TOKEN": "do-not-render"},
            },
            "dormant": {"type": "local", "command": ["not-started"], "disabled": true},
        }},
    });
    fs::write(
        config.join("grapt.jsonc"),
        serde_json::to_string(&user_config)?,
    )?;
    let project = root.join("project");
    fs::create_dir_all(&project)?;
    let project_config = serde_json::json!({
        "mcp": {"servers": {
            "repo-dormant": {"type": "local", "command": ["not-started"], "disabled": true},
        }},
    });
    fs::write(
        project.join("grapt.jsonc"),
        serde_json::to_string(&project_config)?,
    )?;

    let provider = ProviderFixture::start(vec![]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let query = format!(
        "/api/mcp/configuration?location%5Bdirectory%5D={}",
        encode_path(&project.to_string_lossy())
    );
    let response = json(address, "GET", &query, "");
    let rows = response["data"]
        .as_array()
        .ok_or("configuration rows are an array")?;
    let row = |name: &str| -> Result<&serde_json::Value, Box<dyn std::error::Error>> {
        rows.iter()
            .find(|candidate| candidate["name"] == name)
            .ok_or_else(|| format!("no row for {name} in {rows:?}").into())
    };
    assert_eq!(row("declared")?["scope"], "user");
    assert_eq!(row("declared")?["disabled"], false);
    assert_eq!(row("dormant")?["scope"], "user");
    assert_eq!(row("dormant")?["disabled"], true);
    assert_eq!(row("repo-dormant")?["scope"], "project");
    assert_eq!(row("repo-dormant")?["disabled"], true);
    // Give a would-be startup its bounded head start, then prove none ran.
    std::thread::sleep(Duration::from_millis(500));
    assert!(!marker.exists(), "the configuration route started a peer");
    // The DTO is names and flags only: no environment value in the body.
    let body = response.to_string();
    assert!(!body.contains("do-not-render"), "{body}");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}

use std::time::Duration;

#[tokio::test]
async fn mcp_public_binary_exposes_help_and_requires_an_explicit_service(
) -> Result<(), Box<dyn std::error::Error>> {
    for (arguments, code, fragment) in [
        (vec!["mcp", "--help"], 0, "mcp login"),
        (vec!["mcp", "login", "docs"], 2, "--service"),
        (vec!["mcp", "list"], 2, "--service"),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
                .args(arguments)
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        assert_eq!(output.status.code(), Some(code));
        let mut text = String::from_utf8(output.stdout)?;
        text.push_str(&String::from_utf8(output.stderr)?);
        assert!(text.contains(fragment), "{text}");
    }
    Ok(())
}

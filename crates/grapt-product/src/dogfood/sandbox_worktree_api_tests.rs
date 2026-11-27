//! Public HTTP regression; declared under session_config::tests to reuse its fixtures.
use super::{ingress, Root, TestResult};
use grapt_api::u3a::{SandboxWorktreeConfigRequestDto, SandboxWorktreeConfigResponseDto};
use serde_json::{json, Value};

#[tokio::test]
async fn worktree_warning_and_consented_repair_cross_authenticated_ingress() -> TestResult {
    let root = Root::new()?;
    let project = root.0.join("project");
    for args in [
        &["init", "--quiet"][..],
        &["config", "extensions.worktreeConfig", "true"][..],
    ] {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let config = root.0.join("service/grapt.jsonc");
    let deny = br#"{"sandbox":{"enabled":true},"permission":{"edit":"deny","shell":"deny"}}"#;
    std::fs::write(&config, deny)?;
    let git_config = std::fs::read(project.join(".git/config"))?;
    let expected = std::fs::canonicalize(project.join(".git"))?.join("config.worktree");
    let (app, _driver) = root.open()?;
    let created: Value = ingress::page(
        &ingress::call(
            &app,
            "POST",
            "/api/session",
            &json!({"location":{"directory":project}}).to_string(),
        )
        .await?,
    )?;
    let session = created["data"]["id"].as_str().ok_or("session id")?;
    let history = format!("/api/session/{session}/message?order=asc");
    let startup: Value = ingress::page(&ingress::call(&app, "GET", &history, "").await?)?;
    let warning = startup["data"]
        .as_array()
        .ok_or("history rows")?
        .iter()
        .any(|row| {
            row["metadata"]["sandboxWorktreeConfig"] == true
                && row["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("config.worktree"))
        });
    assert_eq!(warning, cfg!(windows), "startup warning: {startup}");
    let route = format!("/api/session/{session}/sandbox/worktree-config");
    let preview: SandboxWorktreeConfigResponseDto =
        ingress::page(&ingress::call(&app, "GET", &route, "").await?)?;
    let preview = serde_json::to_value(preview)?;
    assert_eq!(preview["data"]["missing"], cfg!(windows));
    assert!(
        !expected.exists(),
        "startup and preview must remain read-only"
    );
    let request = |consent| -> TestResult<String> {
        let dto: SandboxWorktreeConfigRequestDto =
            serde_json::from_value(json!({"consent":consent,"path":expected}))?;
        Ok(serde_json::to_string(&dto)?)
    };
    let refused = ingress::call(&app, "POST", &route, &request(false)?).await?;
    assert!(
        refused.starts_with(b"HTTP/1.1 400"),
        "{}",
        String::from_utf8_lossy(&refused)
    );
    assert!(!expected.exists(), "refused consent must not create a file");
    let repaired = ingress::call(&app, "POST", &route, &request(true)?).await?;
    if cfg!(windows) {
        assert_eq!(preview["data"]["path"].as_str(), expected.to_str());
        let repaired: SandboxWorktreeConfigResponseDto = ingress::page(&repaired)?;
        assert_eq!(serde_json::to_value(repaired)?["data"]["missing"], false);
        assert_eq!(std::fs::metadata(&expected)?.len(), 0);
        let recheck: SandboxWorktreeConfigResponseDto =
            ingress::page(&ingress::call(&app, "GET", &route, "").await?)?;
        assert_eq!(serde_json::to_value(recheck)?["data"]["missing"], false);
    } else {
        assert!(
            repaired.starts_with(b"HTTP/1.1 400"),
            "{}",
            String::from_utf8_lossy(&repaired)
        );
        assert!(String::from_utf8_lossy(&repaired).contains("not enabled"));
        assert!(
            !expected.exists(),
            "unsupported platforms must never repair"
        );
    }
    assert_eq!(std::fs::read(&config)?, deny);
    assert_eq!(std::fs::read(project.join(".git/config"))?, git_config);
    Ok(())
}

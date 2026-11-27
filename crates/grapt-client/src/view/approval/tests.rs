use super::*;
use crate::{client::PermissionAsk, view::permission};

#[test]
fn a_patch_is_coloured_by_line() -> Result<(), Box<dyn std::error::Error>> {
    let patch = "--- a.rs\n+++ a.rs\n@@ replacement @@\n*** Begin Patch\n-old\n+new\n\\ No newline at end of input\ncontext";
    let ask = PermissionAsk {
        session_id: crate::wire::session_id("ses_preview")?,
        request_id: crate::wire::permission_id("per_preview")?,
        title: "patch".into(),
        subtitle: "tool patch".into(),
        live: true,
        allow_always: true,
        call: None,
        preview: Some(serde_json::from_value(serde_json::json!({"type":"patch",
            "format":"replacement","patch":patch,"truncated":false}))?),
    };
    let rows = permission::review_rows(&ask, 80);
    for (text, style) in [
        ("--- a.rs", Style::token(Token::Accent)),
        ("+++ a.rs", Style::token(Token::Accent)),
        ("@@ replacement @@", Style::token(Token::Accent)),
        ("*** Begin Patch", Style::token(Token::Accent)),
        ("-old", Style::token(Token::DiffDel)),
        ("+new", Style::token(Token::DiffAdd)),
        ("\\ No newline at end of input", Style::token(Token::Muted)),
        ("context", Style::PLAIN),
    ] {
        assert!(rows.iter().any(|(row, role)| row == text && *role == style));
    }
    Ok(())
}

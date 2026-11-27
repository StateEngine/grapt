//! Approval hooks exercise the real permission/effect path, including reopen.
use super::*;

#[test]
fn user_permission_hook_allow_is_one_call_and_deny_reaches_the_model(
) -> Result<(), Box<dyn std::error::Error>> {
    let (_, text, called) = v2_tool_call("request-allow")?;
    assert!(called);
    assert_eq!(text, "changed");
    let (output, text, called) = v2_tool_call("request-deny")?;
    assert!(called);
    assert_eq!(text, "original");
    assert!(
        output.contains("approval hook refused this edit"),
        "{output}"
    );
    Ok(())
}

#[test]
fn permission_hook_allow_cannot_lift_denies_projects_or_forced_asks(
) -> Result<(), Box<dyn std::error::Error>> {
    for mode in ["request-ceiling", "request-project", "request-pre-ask"] {
        let (output, text, called) = v2_tool_call(mode)?;
        assert_eq!(text, "original", "{mode}: {output}");
        assert_eq!(called, mode != "request-ceiling", "{mode}");
        assert!(
            output.contains(if mode == "request-ceiling" {
                "denied"
            } else {
                "approval unavailable"
            }),
            "{mode}: {output}"
        );
    }
    Ok(())
}

#[test]
fn unsupported_permission_updates_and_timeout_never_dispatch(
) -> Result<(), Box<dyn std::error::Error>> {
    for mode in ["request-invalid", "request-timeout"] {
        let (output, text, called) = v2_tool_call(mode)?;
        assert!(called);
        assert_eq!(text, "original");
        assert!(
            output.contains("PermissionRequest hook refused; no effect ran"),
            "{mode}: {output}"
        );
    }
    Ok(())
}

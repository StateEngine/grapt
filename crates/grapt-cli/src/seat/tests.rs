use super::*;

#[test]
fn selected_config_never_falls_back_from_an_explicit_invalid_modern_path() -> Result<(), String> {
    let path = if cfg!(windows) {
        "E:/private/seat.json"
    } else {
        "/private/seat.json"
    };
    assert_eq!(selected_path(None, None)?, None);
    // The path is opaque to a remote provisioner; its host need not match the service.
    assert_eq!(
        selected_path(Some("E:/private/remote.json".into()), None)?,
        Some("E:/private/remote.json".into())
    );
    assert_eq!(
        selected_path(Some("/private/remote.json".into()), None)?,
        Some("/private/remote.json".into())
    );
    assert_eq!(selected_path(None, Some(path.into()))?, Some(path.into()));
    assert_eq!(
        selected_path(Some(path.into()), Some("relative".into()))?,
        Some(path.into())
    );
    for invalid in ["", "relative", "line\nbreak"] {
        assert!(selected_path(Some(invalid.into()), Some(path.into())).is_err());
    }
    Ok(())
}

fn arguments() -> Vec<String> {
    ["provision", "--service", "/profile", "--session", "ses_launch", "--agent", "build",
        "--model", "vendor/model", "--location", "/repo", "--identity-entry",
        r#"{"scope":"project","project_id":"fixture","agent_id":"agt_seat","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#,
        "--packet-entry", r#"{"state":"written","path":"/packet","bytes":1,"sha256":"a"}"#]
        .into_iter().map(str::to_owned).collect()
}

#[test]
fn explicit_provision_grammar_keeps_product_and_external_identities_distinct(
) -> Result<(), Box<dyn std::error::Error>> {
    let command = ProvisionCommand::parse(&arguments())?;
    let value = serde_json::to_value(command.create()?)?;
    assert_eq!(
        value,
        json!({"id":"ses_launch","agent":"build","model":{"providerID":"vendor","id":"model"},"location":{"directory":"/repo"}})
    );
    let (identity, _) = command.entries().map_err(|_| "entries")?;
    assert_eq!(
        serde_json::to_value(identity)?["value"]["agent_id"],
        "agt_seat"
    );
    let mut full = vec!["session".to_owned()];
    full.extend(arguments());
    assert!(matches!(
        crate::CliCommand::parse(full),
        Ok(crate::CliCommand::SessionProvision(_))
    ));
    Ok(())
}

#[test]
fn malformed_or_ambiguous_launch_input_is_refused_before_writes() {
    for (index, value) in [
        (2, ""),
        (4, "not-session"),
        (6, ""),
        (8, "no-provider"),
        (
            12,
            r#"{"scope":"project","project_id":"p","project_id":"q"}"#,
        ),
        (
            12,
            r#"{"scope":"incognito","project_id":"p","agent_id":"a"}"#,
        ),
        (
            12,
            r#"{"scope":"project","project_id":"p","agent_id":"a","mail_transport_protocol":"future"}"#,
        ),
        (14, r#"{"state":"REFUSED"}"#),
        (14, "null"),
    ] {
        let mut input = arguments();
        input[index] = value.into();
        assert!(ProvisionCommand::parse(&input).is_err());
    }
    let mut duplicate = arguments();
    duplicate[13] = "--identity-entry".into();
    assert!(matches!(
        ProvisionCommand::parse(&duplicate),
        Err(CommandError::Duplicate("--identity-entry"))
    ));
    let mut oversized = arguments();
    oversized[14] = "x".repeat(MAX_ENTRY + 1);
    assert!(ProvisionCommand::parse(&oversized).is_err());
}

#[test]
fn capability_input_accepts_one_line_and_never_reports_secret_bytes() -> Result<(), String> {
    for ending in ["", "\n", "\r\n"] {
        let capability = stdin_capability(format!("sec1_private{ending}").into_bytes())?;
        assert_eq!(capability.expose(), "sec1_private");
        assert!(!format!("{capability:?}").contains("private"));
    }
    for bytes in [
        vec![],
        b"sec1_private\nsecond".to_vec(),
        vec![b'x'; MAX_CAPABILITY + 1],
        vec![0xff],
    ] {
        let error = stdin_capability(bytes)
            .err()
            .ok_or("invalid input accepted")?;
        assert!(!error.contains("private"));
    }
    Ok(())
}

#[tokio::test]
async fn capability_reader_rejects_trailing_bytes_after_a_maximum_crlf_line() -> Result<(), String>
{
    let capability = "x".repeat(MAX_CAPABILITY);
    for suffix in ["", "\n", "\r\n"] {
        let input = format!("{capability}{suffix}");
        assert_eq!(
            read_capability(input.as_bytes()).await?.expose(),
            &capability
        );
    }
    for suffix in ["\r\nx", "\r\nextra", "\nsecond", "x"] {
        let input = format!("{capability}{suffix}");
        assert!(read_capability(input.as_bytes()).await.is_err());
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn capability_reader_times_out_an_open_stdin_without_creating_a_session() {
    let (_writer, reader) = tokio::io::duplex(1);
    let error = read_capability(reader).await.err();
    assert_eq!(error.as_deref(), Some("capability stdin timed out"));
}

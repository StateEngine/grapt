use super::*;
use grapt_login::chatgpt;

pub(super) fn signed_source(scratch: &Scratch) -> Result<std::path::PathBuf, String> {
    let source = scratch.account()?;
    let config = String::from_utf8(capture(&source.join("grapt.jsonc"), CONFIG_BYTES)?)
        .map_err(|error| error.to_string())?;
    std::fs::write(
        source.join("grapt.jsonc"),
        config.replace("test-v1", chatgpt::SIGN_IN_CREDENTIAL_VERSION),
    )
    .map_err(|error| error.to_string())?;
    chatgpt::save(
        &source,
        &chatgpt::Tokens {
            access: "IR_DUMMY_ACCESS".into(),
            refresh: "IR_DUMMY_REFRESH".into(),
            expires_at: grapt_types::Timestamp::from_millis(2_000_000_000_000),
            account: "test-account".into(),
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(source)
}

#[test]
fn ir_import_rejects_invalid_or_mixed_sign_in_without_publishing_or_leaking() -> Result<(), String>
{
    for fault in [
        "malformed",
        "directory",
        "oversize",
        "foreign-account",
        "access-mismatch",
    ] {
        let scratch = Scratch::new(fault)?;
        let source = signed_source(&scratch)?;
        let path = source.join(chatgpt::STATE_FILE);
        match fault {
            "malformed" => std::fs::write(&path, b"IR_DUMMY_REFRESH {"),
            "directory" => {
                std::fs::remove_file(&path).map_err(|error| error.to_string())?;
                std::fs::create_dir(&path)
            }
            "oversize" => std::fs::write(&path, vec![b'X'; 16 * 1024 + 1]),
            "foreign-account" => {
                let mut tokens = chatgpt::load(&source)
                    .map_err(|error| error.to_string())?
                    .ok_or("fixture sign-in")?;
                tokens.account = "foreign-account".into();
                chatgpt::save(&source, &tokens).map_err(|error| error.to_string())?;
                Ok(())
            }
            _ => std::fs::write(source.join(chatgpt::CREDENTIAL_FILE), b"IR_OTHER_ACCESS"),
        }
        .map_err(|error| error.to_string())?;
        let config = capture(&source.join("grapt.jsonc"), CONFIG_BYTES)?;
        let access = capture(&source.join(chatgpt::CREDENTIAL_FILE), CONFIG_BYTES)?;
        let target = scratch.0.join("new-profile");
        let error = initialize(&target, &source)
            .err()
            .ok_or("invalid sign-in imported")?;
        assert!(!target.exists(), "invalid sign-in published at {fault}");
        for secret in ["IR_DUMMY_ACCESS", "IR_DUMMY_REFRESH", "IR_OTHER_ACCESS"] {
            assert!(
                !error.contains(secret),
                "sign-in error leaked a credential at {fault}"
            );
        }
        assert_eq!(capture(&source.join("grapt.jsonc"), CONFIG_BYTES)?, config);
        assert_eq!(
            capture(&source.join(chatgpt::CREDENTIAL_FILE), CONFIG_BYTES)?,
            access
        );
    }
    Ok(())
}

#[test]
fn ir_import_preserves_fresh_sign_in_and_api_key_setup_paths() -> Result<(), String> {
    let scratch = Scratch::new("first-sign-in")?;
    let source = signed_source(&scratch)?;
    std::fs::remove_file(source.join(chatgpt::STATE_FILE)).map_err(|error| error.to_string())?;
    let target = scratch.0.join("new-profile");
    initialize(&target, &source)?;
    assert!(chatgpt::load(&target)
        .map_err(|error| error.to_string())?
        .is_none());

    let scratch = Scratch::new("api-key")?;
    let source = scratch.account()?;
    std::fs::write(
        source.join(chatgpt::STATE_FILE),
        b"unrelated malformed state",
    )
    .map_err(|error| error.to_string())?;
    let target = scratch.0.join("new-profile");
    initialize(&target, &source)?;
    assert!(
        !target.join(chatgpt::STATE_FILE).exists(),
        "API-key setup imported unrelated sign-in"
    );
    Ok(())
}

#[test]
fn ir_import_retains_the_complete_account_pinned_chatgpt_sign_in() -> Result<(), String> {
    let scratch = Scratch::new("sign-in")?;
    let source = signed_source(&scratch)?;
    let original = capture(&source.join(chatgpt::STATE_FILE), 16 * 1024)?;
    let target = scratch.0.join("new-profile");
    initialize(&target, &source)?;
    let imported = chatgpt::load(&target)
        .map_err(|error| error.to_string())?
        .ok_or("import silently lost the sign-in generation")?;
    assert_eq!(imported.access, "IR_DUMMY_ACCESS");
    assert_eq!(imported.refresh, "IR_DUMMY_REFRESH");
    assert_eq!(imported.account, "test-account");
    assert_eq!(imported.expires_at.as_millis(), 2_000_000_000_000);
    assert!(chatgpt::Session::open(&target, "test-account")
        .map_err(|error| error.to_string())?
        .is_some());
    assert_eq!(
        capture(&source.join(chatgpt::STATE_FILE), 16 * 1024)?,
        original
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(target.join(chatgpt::STATE_FILE))
                .map_err(|error| error.to_string())?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    Ok(())
}

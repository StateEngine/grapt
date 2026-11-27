//! Public sign-in persistence and migration on fixture profiles only.
use super::*;

#[test]
fn save_and_load_keep_both_token_files_protected_on_the_atomic_path() {
    let profile = Profile::new("dpapi-save");
    let first = signed_in(NOW + HOUR);
    save(&profile.0, &first).unwrap_or_else(|error| panic!("save: {error}"));
    for name in [STATE_FILE, CREDENTIAL_FILE] {
        let bytes =
            std::fs::read(profile.0.join(name)).unwrap_or_else(|error| panic!("read: {error}"));
        assert!(crate::profile_security::is_protected(&bytes));
        assert!(!bytes
            .windows(first.refresh.len())
            .any(|part| part == first.refresh.as_bytes()));
    }
    crate::profile_security::verify_directory(&profile.0)
        .unwrap_or_else(|error| panic!("DACL: {error}"));
    let recovered = load(&profile.0)
        .unwrap_or_else(|error| panic!("load: {error}"))
        .unwrap_or_else(|| panic!("missing generation"));
    assert_eq!(recovered.refresh, first.refresh);
    assert_eq!(recovered.access, first.access);

    let original =
        std::fs::read(profile.0.join(STATE_FILE)).unwrap_or_else(|error| panic!("read: {error}"));
    let temporary = profile
        .0
        .join(format!(".{STATE_FILE}.{}.new", std::process::id()));
    std::fs::write(&temporary, b"not owned by replacement")
        .unwrap_or_else(|error| panic!("blocker: {error}"));
    assert!(save(&profile.0, &signed_in(NOW + 2 * HOUR)).is_err());
    assert_eq!(
        std::fs::read(profile.0.join(STATE_FILE)).unwrap_or_else(|error| panic!("read: {error}")),
        original
    );
    assert_eq!(
        std::fs::read(temporary).unwrap_or_else(|error| panic!("read: {error}")),
        b"not owned by replacement"
    );
}

#[tokio::test]
async fn startup_migrates_legacy_tokens_before_any_provider_request() {
    let profile = Profile::new("dpapi-migration");
    crate::profile_security::protect_directory(&profile.0)
        .unwrap_or_else(|error| panic!("private migration fixture: {error}"));
    // Stay outside the 48-hour launch-renewal horizon: this fixture proves
    // migration without any provider request, not a deferred refresh.
    let tokens = signed_in(NOW + 72 * HOUR);
    std::fs::write(profile.0.join(STATE_FILE), encode(&tokens))
        .unwrap_or_else(|error| panic!("state: {error}"));
    std::fs::write(profile.0.join(CREDENTIAL_FILE), tokens.access.as_bytes())
        .unwrap_or_else(|error| panic!("projection: {error}"));
    let client = client();
    let outcome = refresh_if_due(&profile.0, &client, "http://127.0.0.1:1", &Fixed(NOW)).await;
    assert!(matches!(outcome, Ok(Renewal::Current)), "{outcome:?}");
    for name in [STATE_FILE, CREDENTIAL_FILE] {
        assert!(crate::profile_security::is_protected(
            &std::fs::read(profile.0.join(name)).unwrap_or_else(|error| panic!("read: {error}"))
        ));
    }
    assert_eq!(profile.read(CREDENTIAL_FILE), tokens.access);
}

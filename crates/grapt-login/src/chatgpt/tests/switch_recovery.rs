//! A sign-in switch interrupted at either side of its commit leaves one
//! usable generation: the old one before the marker, the new one after.
use grapt_types::Timestamp;

use super::super::{load, own, save, switch, SwitchFile, Tokens, CREDENTIAL_FILE};
use super::{access, signed_in, Profile, HOUR, NOW};

fn switched_to_b() -> Tokens {
    Tokens {
        access: access("account-b", NOW + 2 * HOUR),
        refresh: "refresh-b".to_owned(),
        expires_at: Timestamp::from_millis(NOW + 2 * HOUR),
        account: "account-b".to_owned(),
    }
}

fn files() -> Vec<SwitchFile> {
    vec![
        SwitchFile::Config(br#"{"provider":{"account":"account-b"}}"#.to_vec()),
        SwitchFile::Listing(br#"{"models":["b"]}"#.to_vec()),
    ]
}

fn signed_in_as_a(case: &str) -> Profile {
    let profile = Profile::new(case);
    std::fs::write(
        profile.0.join("grapt.jsonc"),
        br#"{"provider":{"account":"account-a"}}"#,
    )
    .unwrap_or_else(|error| panic!("config: {error}"));
    std::fs::write(
        profile.0.join("account-models.json"),
        br#"{"models":["a"]}"#,
    )
    .unwrap_or_else(|error| panic!("listing: {error}"));
    save(&profile.0, &signed_in(NOW + HOUR)).unwrap_or_else(|error| panic!("{error}"));
    profile
}

fn plain(profile: &Profile, name: &str) -> String {
    std::fs::read_to_string(profile.0.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
}

#[tokio::test]
async fn a_switch_interrupted_before_its_commit_keeps_the_old_generation() {
    let profile = signed_in_as_a("switch-uncommitted");
    let journal = profile.0.join(".chatgpt-switch");
    // A crash after staging, before the marker.
    switch::stage(&journal, &switched_to_b(), &files(), false)
        .unwrap_or_else(|error| panic!("stage: {error}"));
    drop(
        own(&profile.0)
            .await
            .unwrap_or_else(|error| panic!("{error}")),
    );
    assert!(!journal.exists());
    assert_eq!(load(&profile.0).ok().flatten(), Some(signed_in(NOW + HOUR)));
    assert_eq!(profile.read(CREDENTIAL_FILE), signed_in(NOW + HOUR).access);
    assert!(plain(&profile, "grapt.jsonc").contains("account-a"));
    assert!(plain(&profile, "account-models.json").contains("\"a\""));
}

#[tokio::test]
async fn a_switch_interrupted_while_publishing_its_marker_keeps_the_old_generation() {
    let profile = signed_in_as_a("switch-marker-cut");
    let journal = profile.0.join(".chatgpt-switch");
    // A crash inside the marker's own write leaves only a partial temporary.
    switch::stage(&journal, &switched_to_b(), &files(), false)
        .unwrap_or_else(|error| panic!("stage: {error}"));
    std::fs::write(journal.join("committed.new"), b"grapt.cha")
        .unwrap_or_else(|error| panic!("partial marker: {error}"));
    drop(
        own(&profile.0)
            .await
            .unwrap_or_else(|error| panic!("{error}")),
    );
    assert!(!journal.exists());
    assert_eq!(load(&profile.0).ok().flatten(), Some(signed_in(NOW + HOUR)));
    assert!(plain(&profile, "grapt.jsonc").contains("account-a"));
}

#[tokio::test]
async fn a_journal_holding_anything_unexpected_is_refused_and_kept() {
    let profile = signed_in_as_a("switch-foreign");
    let journal = profile.0.join(".chatgpt-switch");
    switch::stage(&journal, &switched_to_b(), &files(), true)
        .unwrap_or_else(|error| panic!("stage: {error}"));
    std::fs::write(journal.join("note.txt"), b"someone's note")
        .unwrap_or_else(|error| panic!("note: {error}"));
    assert!(own(&profile.0).await.is_err());
    // Nothing published, nothing removed.
    assert_eq!(
        std::fs::read(journal.join("note.txt")).unwrap_or_default(),
        b"someone's note"
    );
    assert!(journal.join("committed").exists());
    assert_eq!(load(&profile.0).ok().flatten(), Some(signed_in(NOW + HOUR)));
    assert!(plain(&profile, "grapt.jsonc").contains("account-a"));
}

#[tokio::test]
async fn a_switch_interrupted_after_its_commit_is_finished_by_the_next_owner() {
    let profile = signed_in_as_a("switch-committed");
    let journal = profile.0.join(".chatgpt-switch");
    // A crash after the marker, before any file was published.
    switch::stage(&journal, &switched_to_b(), &files(), true)
        .unwrap_or_else(|error| panic!("stage: {error}"));
    drop(
        own(&profile.0)
            .await
            .unwrap_or_else(|error| panic!("{error}")),
    );
    assert!(!journal.exists());
    assert_eq!(load(&profile.0).ok().flatten(), Some(switched_to_b()));
    assert_eq!(profile.read(CREDENTIAL_FILE), switched_to_b().access);
    assert!(plain(&profile, "grapt.jsonc").contains("account-b"));
    assert!(plain(&profile, "account-models.json").contains("\"b\""));
}

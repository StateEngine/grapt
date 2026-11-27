#[path = "../../../../test-support/commit_fault.rs"]
mod commit_fault;
use super::*;
use std::path::PathBuf;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Database(PathBuf);

impl Database {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "grapt-mcp-credential-{}",
            crate::OsRandomIds::seed()?.instance()
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn open(&self) -> Result<EventStore, WriteError> {
        EventStore::open(&self.0.join("store.db"), "0.1.0")
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn key() -> Result<McpCredentialKey, WriteError> {
    McpCredentialKey::for_server("docs", "https://example.com/mcp?credential=secret")
}

fn token(value: &str) -> Option<Secret<String>> {
    Some(Secret::new(value.to_owned()))
}

#[test]
fn keys_bind_alias_full_url_and_field_boundaries_without_debug_secrets() -> TestResult {
    let key = key()?;
    assert!(!format!("{key:?}").contains("secret"));
    for (server, endpoint) in [
        ("other", "https://example.com/mcp?credential=secret"),
        ("docs", "https://example.com/other?credential=secret"),
        ("docs", "https://example.com/mcp?credential=other"),
    ] {
        assert_ne!(
            key.sql_key(),
            McpCredentialKey::for_server(server, endpoint)?.sql_key()
        );
    }
    assert_ne!(
        McpCredentialKey::for_server("ab", "c")?.sql_key(),
        McpCredentialKey::for_server("a", "bc")?.sql_key()
    );
    assert!(McpCredentialKey::for_server("", "https://example.com").is_err());
    assert!(McpCredentialKey::for_server("docs", &"x".repeat(8193)).is_err());
    Ok(())
}

#[test]
fn credentials_and_sign_out_tombstones_survive_reopening() -> TestResult {
    let database = Database::new()?;
    let key = key()?;
    let mut store = database.open()?;
    let initial = store.mcp_credential(&key)?;
    assert_eq!(initial.revision, McpCredentialRevision::Missing);
    assert!(initial.value.is_none());
    let written =
        store.replace_mcp_credential(&key, initial.revision, token("access-and-refresh"))?;
    assert_eq!(written.revision, McpCredentialRevision::Stored(1));
    assert!(!format!("{written:?}").contains("access-and-refresh"));
    drop(store);
    let mut reopened = database.open()?;
    let held = reopened.mcp_credential(&key)?;
    assert_eq!(
        held.value.as_ref().map(Secret::expose).map(String::as_str),
        Some("access-and-refresh")
    );
    assert!(reopened
        .integration_credential_for_plugin(&key.sql_key(), "untrusted-plugin")?
        .is_none());
    reopened.replace_mcp_credential(&key, held.revision, None)?;
    drop(reopened);
    let final_store = database.open()?;
    let logged_out = final_store.mcp_credential(&key)?;
    assert_eq!(logged_out.revision, McpCredentialRevision::Stored(2));
    assert!(logged_out.value.is_none());
    Ok(())
}

#[test]
fn stale_login_refresh_and_rejection_cannot_resurrect_or_erase_credentials() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let key = key()?;
    let old_login = store.mcp_credential(&key)?.revision;
    let logged_out = store.replace_mcp_credential(&key, old_login, None)?;
    assert!(matches!(
        store.replace_mcp_credential(&key, old_login, token("late-login")),
        Err(WriteError::McpCredentialConflict)
    ));
    let login = store.replace_mcp_credential(&key, logged_out.revision, token("new-login"))?;
    assert!(matches!(
        store.replace_mcp_credential(&key, logged_out.revision, None),
        Err(WriteError::McpCredentialConflict)
    ));
    let refreshed = store.replace_mcp_credential(&key, login.revision, token("rotated"))?;
    assert!(matches!(
        store.replace_mcp_credential(&key, login.revision, token("stale-refresh")),
        Err(WriteError::McpCredentialConflict)
    ));
    assert!(matches!(
        store.replace_mcp_credential(&key, login.revision, None),
        Err(WriteError::McpCredentialConflict)
    ));
    assert_eq!(
        store
            .mcp_credential(&key)?
            .value
            .as_ref()
            .map(Secret::expose)
            .map(String::as_str),
        Some("rotated")
    );
    store.replace_mcp_credential(&key, refreshed.revision, None)?;
    assert!(matches!(
        store.replace_mcp_credential(&key, refreshed.revision, token("late-refresh")),
        Err(WriteError::McpCredentialConflict)
    ));
    assert!(store.mcp_credential(&key)?.value.is_none());
    Ok(())
}

#[test]
fn independent_connections_compare_against_the_committed_winner() -> TestResult {
    let database = Database::new()?;
    let key = key()?;
    let mut first = database.open()?;
    let mut second = database.open()?;
    let first_view = first.mcp_credential(&key)?.revision;
    let second_view = second.mcp_credential(&key)?.revision;
    first.replace_mcp_credential(&key, first_view, token("winner"))?;
    assert!(matches!(
        second.replace_mcp_credential(&key, second_view, token("loser")),
        Err(WriteError::McpCredentialConflict)
    ));
    assert_eq!(
        second
            .mcp_credential(&key)?
            .value
            .as_ref()
            .map(Secret::expose)
            .map(String::as_str),
        Some("winner")
    );
    Ok(())
}

#[test]
fn failed_commit_keeps_the_previous_revision_and_credential() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let key = key()?;
    let original =
        store.replace_mcp_credential(&key, McpCredentialRevision::Missing, token("old-secret"))?;
    let fault = commit_fault::CommitFault::install(
        &mut store,
        rusqlite::hooks::Action::SQLITE_UPDATE,
        "mcp_oauth_credential",
        0,
    )?;
    let failure = store
        .replace_mcp_credential(&key, original.revision, token("new-secret"))
        .err()
        .ok_or("injected failure did not fire")?;
    let detail = failure.to_string();
    assert!(!detail.contains("old-secret") && !detail.contains("new-secret"));
    let held = store.mcp_credential(&key)?;
    assert_eq!(held.revision, original.revision);
    assert_eq!(
        held.value.as_ref().map(Secret::expose).map(String::as_str),
        Some("old-secret")
    );
    drop(fault);
    store.replace_mcp_credential(&key, original.revision, token("new-secret"))?;
    Ok(())
}

#[test]
fn raw_writes_are_denied_by_the_installed_authorizer() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    store.replace_mcp_credential(&key()?, McpCredentialRevision::Missing, token("held"))?;
    for sql in [
        "UPDATE mcp_oauth_credential SET value = NULL",
        "DELETE FROM mcp_oauth_credential",
        "INSERT INTO mcp_oauth_credential (key, revision, value) VALUES ('forged', 1, NULL)",
    ] {
        match store.connection().execute(sql, []) {
            Err(rusqlite::Error::SqliteFailure(code, _))
                if code.code == rusqlite::ErrorCode::AuthorizationForStatementDenied => {}
            result => panic!("MCP credential bypass was not denied: {result:?}"),
        }
    }
    assert!(store.mcp_credential(&key()?)?.value.is_some());
    Ok(())
}

#[test]
fn oversized_values_and_exhausted_revisions_are_refused() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let key = key()?;
    assert!(matches!(
        store.replace_mcp_credential(
            &key,
            McpCredentialRevision::Missing,
            token(&"x".repeat(MAX_MCP_CREDENTIAL_BYTES + 1))
        ),
        Err(WriteError::McpCredentialInput)
    ));
    assert!(matches!(
        store.replace_mcp_credential(&key, McpCredentialRevision::Stored(i64::MAX as u64), None),
        Err(WriteError::McpCredentialExhausted)
    ));
    assert!(store.mcp_credential(&key)?.value.is_none());
    assert_eq!(
        store.mcp_credential(&key)?.revision,
        McpCredentialRevision::Missing
    );
    Ok(())
}

#[test]
fn corrupt_credential_bytes_are_refused_instead_of_becoming_absent() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let key = key()?;
    store.replace_mcp_credential(&key, McpCredentialRevision::Missing, token("held"))?;
    // Simulate an externally damaged database through the governed fixture
    // owner. Production writes keep CHECK constraints enabled.
    store
        .connection()
        .execute_batch("PRAGMA ignore_check_constraints = ON")?;
    for invalid in [
        "zeroblob(65537)",
        "CAST(x'ff' AS TEXT)",
        "''",
        "zeroblob(0)",
    ] {
        let tx = store.governed_transaction()?;
        tx.gate().execute(
            &tx,
            GovernedTable::McpOAuthCredential,
            &format!("UPDATE mcp_oauth_credential SET value = {invalid} WHERE key = ?1"),
            [key.sql_key()],
        )?;
        tx.commit()?.discard();
        assert!(
            matches!(
                store.mcp_credential(&key),
                Err(WriteError::McpCredentialCorrupt)
            ),
            "invalid credential fixture was accepted: {invalid}"
        );
    }
    Ok(())
}

#[test]
fn abrupt_process_exit_after_logout_keeps_the_tombstone_and_refuses_stale_login() -> TestResult {
    use std::process::{Command, Stdio};
    let database = Database::new()?;
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--ignored",
            "--exact",
            "mcp_credential::tests::logout_crash_child",
        ])
        .env("GRAPT_MCP_LOGOUT_CRASH_DB", database.0.join("store.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut observed = None;
    for _ in 0..1000 {
        if let Some(status) = child.try_wait()? {
            observed = Some(status);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let Some(status) = observed else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("credential crash child did not reach its cut within ten seconds".into());
    };
    assert_eq!(
        status.code(),
        Some(87),
        "the child must reach the abrupt post-commit exit"
    );
    let mut store = database.open()?;
    let key = key()?;
    let held = store.mcp_credential(&key)?;
    assert_eq!(held.revision, McpCredentialRevision::Stored(2));
    assert!(held.value.is_none());
    assert!(matches!(
        store.replace_mcp_credential(&key, McpCredentialRevision::Missing, token("late-login")),
        Err(WriteError::McpCredentialConflict)
    ));
    assert!(matches!(
        store.replace_mcp_credential(
            &key,
            McpCredentialRevision::Stored(1),
            token("late-refresh")
        ),
        Err(WriteError::McpCredentialConflict)
    ));
    assert!(store.mcp_credential(&key)?.value.is_none());
    Ok(())
}

/// Only the parent witness invokes this ignored fixture. Missing coordinates
/// fail loudly; the parent requires exit 87, proving this exact cut executed.
#[test]
#[ignore = "subprocess fixture invoked by abrupt_process_exit_after_logout"]
fn logout_crash_child() -> TestResult {
    let path = std::env::var_os("GRAPT_MCP_LOGOUT_CRASH_DB")
        .ok_or("logout crash fixture requires its database path")?;
    let mut store = EventStore::open(std::path::Path::new(&path), "0.1.0")?;
    let key = key()?;
    let login =
        store.replace_mcp_credential(&key, McpCredentialRevision::Missing, token("old-login"))?;
    store.replace_mcp_credential(&key, login.revision, None)?;
    // Exit without running store/connection destructors, immediately after
    // commit. Reopening in the parent must observe the durable logout.
    std::process::exit(87)
}

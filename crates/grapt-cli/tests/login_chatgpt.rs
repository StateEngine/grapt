//! `grapt login chatgpt` end to end: the shipped binary signs in against a
//! loopback issuer and model backend and switches an existing profile to
//! another ChatGPT account in place.
#![cfg(feature = "loopback-testkit")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use grapt_login::chatgpt;

/// An unsigned JWT naming account `workspace-b`.
const ID_TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJ3b3Jrc3BhY2UtYiJ9.sig";

const CONFIG: &str = r#"{
  // The comment and layout must survive the switch.
  "catalog": {"source": "models.dev-chatgpt"},
  "provider": {"id": "openai", "model": "gpt-6.1-sol", "account": "workspace-a",
               "credential_version": "chatgpt-oauth-v1", "route": "responses"}
}
"#;

/// Answers each request by path and records the paths it saw.
fn fake_chatgpt() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|error| panic!("{error}"));
    let base = format!(
        "http://{}",
        listener
            .local_addr()
            .unwrap_or_else(|error| panic!("{error}"))
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let path = line.split(' ').nth(1).unwrap_or_default().to_owned();
            let mut length = 0;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let answer = match path.split('?').next().unwrap_or_default() {
                "/api/accounts/deviceauth/usercode" => {
                    r#"{"device_auth_id":"device-1","user_code":"ABCD-1234","interval":"1"}"#
                        .to_owned()
                }
                "/api/accounts/deviceauth/token" => {
                    r#"{"authorization_code":"code-1","code_verifier":"verifier-1"}"#.to_owned()
                }
                "/oauth/token" => format!(
                    r#"{{"access_token":"access-b","refresh_token":"refresh-b","id_token":"{ID_TOKEN}","expires_in":3600}}"#
                ),
                "/models" => {
                    r#"{"models":[{"slug":"gpt-6.1-sol","visibility":"list"}]}"#.to_owned()
                }
                _ => String::new(),
            };
            record
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(path.split('?').next().unwrap_or_default().to_owned());
            let status = if answer.is_empty() {
                "404 Not Found"
            } else {
                "200 OK"
            };
            let mut stream = &stream;
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    (base, seen)
}

struct Profile(PathBuf);

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn login_chatgpt_switches_an_existing_profile_to_another_account_in_place() {
    let profile =
        Profile(std::env::temp_dir().join(format!("grapt-login-chatgpt-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&profile.0);
    std::fs::create_dir(&profile.0).unwrap_or_else(|error| panic!("{error}"));
    let write = |name: &str, bytes: &[u8]| {
        std::fs::write(profile.0.join(name), bytes).unwrap_or_else(|error| panic!("{error}"))
    };
    write("grapt.jsonc", CONFIG.as_bytes());
    write("account-models.json", br#"{"models":[]}"#);
    write(
        "database.sqlite3",
        b"store bytes the sign-in must not touch",
    );
    let old = chatgpt::Tokens {
        access: "access-a".to_owned(),
        refresh: "refresh-a".to_owned(),
        expires_at: grapt_types::Timestamp::from_millis(4_000_000_000_000),
        account: "workspace-a".to_owned(),
    };
    chatgpt::save(&profile.0, &old).unwrap_or_else(|error| panic!("{error}"));

    let (base, seen) = fake_chatgpt();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
        .args(["login", "chatgpt", "--profile"])
        .arg(&profile.0)
        .env("GRAPT_TESTKIT_CHATGPT_ISSUER", &base)
        .env("GRAPT_TESTKIT_CHATGPT_BACKEND", &base)
        .env("NO_PROXY", "127.0.0.1")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .output()
        .unwrap_or_else(|error| panic!("{error}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{stderr}");
    assert!(stderr.contains("ABCD-1234"), "{stderr}");
    assert!(stdout.contains("account workspace-b"), "{stdout}");
    assert!(stdout.contains("changed from workspace-a"), "{stdout}");

    let signed_in = chatgpt::load(&profile.0)
        .unwrap_or_else(|error| panic!("{error}"))
        .unwrap_or_else(|| panic!("no sign-in"));
    assert_eq!(
        (signed_in.account.as_str(), signed_in.access.as_str()),
        ("workspace-b", "access-b")
    );
    let read = |name: &str| std::fs::read(profile.0.join(name)).unwrap_or_default();
    assert_eq!(
        String::from_utf8_lossy(&read("grapt.jsonc")),
        CONFIG.replace("\"workspace-a\"", "\"workspace-b\"")
    );
    assert_eq!(
        read("database.sqlite3"),
        b"store bytes the sign-in must not touch"
    );
    assert!(String::from_utf8_lossy(&read("account-models.json")).contains("gpt-6.1-sol"));
    assert_eq!(
        *seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
        [
            "/api/accounts/deviceauth/usercode",
            "/api/accounts/deviceauth/token",
            "/oauth/token",
            "/models",
        ]
    );
}

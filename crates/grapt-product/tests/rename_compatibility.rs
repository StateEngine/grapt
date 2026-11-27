//! Open retained profile material through the shipped product and store APIs.
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::Instant;

use grapt_login::{chatgpt, profile_security};
use grapt_platform::profile_configuration::ProfileApproval;
use grapt_store::mcp_credential::McpCredentialKey;
use grapt_store::{EventStore, RemoteMailboxInput, RemoteMailboxScope};
use grapt_types::remote_mail::*;
use grapt_types::{MailboxBody, MailboxCursor, ProtocolVersion, SeatMessageKind, SeatRecipientId};
use serde_json::{json, Value};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Fixture {
    _temporary: tempfile::TempDir,
    profile: PathBuf,
    home: PathBuf,
    legacy: Value,
}
impl Fixture {
    fn new() -> TestResult<Self> {
        let temporary = tempfile::tempdir()?;
        let home = temporary.path().join("home");
        let profile = temporary.path().join("repository/.secode");
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&profile)?;
        profile_security::protect_directory(&profile)?;
        let legacy: Value = serde_json::from_str(include_str!("rename_compatibility/legacy.json"))?;
        for (name, bytes) in [
            (
                "secode.jsonc",
                legacy["config"]
                    .as_str()
                    .ok_or("config missing")?
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "service.conf",
                legacy["service"]
                    .as_str()
                    .ok_or("service missing")?
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "chatgpt.oauth.json",
                serde_json::to_vec(&legacy["chatgpt"])?,
            ),
            (
                "provider.credential",
                b"SYNTHETIC_LEGACY_ACCESS_NOT_VALID".to_vec(),
            ),
            ("account-models.json", br#"{"models":[{"slug":"fixture-model","visibility":"list","context_window":128000,"supported_reasoning_levels":[]}]}"#.to_vec()),
            ("basic.credential", b"fixture:synthetic-password".to_vec()),
        ] {
            profile_security::replace_private_file(&profile, name, &bytes)?;
        }
        Ok(Self {
            _temporary: temporary,
            profile: fs::canonicalize(profile)?,
            home,
            legacy,
        })
    }

    fn approval(&self) -> TestResult<String> {
        // The fingerprint is fixed material, including the absent secode.json
        // and connections.json names; only the temporary canonical path varies.
        let encoded = json!({"canonical":self.profile,
            "fingerprint":self.legacy["approval_fingerprint"]})
        .to_string();
        Ok(ProfileApproval::decode(&encoded)?.encoded()?)
    }

    fn command(&self) -> TestResult<Command> {
        self.command_with_approval(self.approval()?)
    }

    fn command_with_approval(&self, approval: String) -> TestResult<Command> {
        let mut command =
            grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"));
        command
            .args(["serve-terminal", "--profile-approval"])
            .arg(approval)
            .arg("--service")
            .arg(&self.profile)
            .args(["--port", "0", "--notify-ready"])
            .current_dir(self.profile.parent().ok_or("repository missing")?)
            .env("GRAPT_TEST_HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"));
        for key in [
            "GRAPT_CONFIG",
            "SECODE_CONFIG",
            concat!("OPEN", "CODE_CONFIG"),
            "GRAPT_CONFIG_DIR",
            "SECODE_CONFIG_DIR",
            "GRAPT_CONFIG_CONTENT",
            "SECODE_CONFIG_CONTENT",
            "STATEENGINE_PROJECT_ID",
            "STATEENGINE_AGENT_ID",
        ] {
            command.env_remove(key);
        }
        Ok(command)
    }

    fn current_command(&self) -> TestResult<Command> {
        // Isolate environment selection from retained-profile compatibility.
        let config = self.legacy["config"]
            .as_str()
            .ok_or("config missing")?
            .replace("chatgpt-oauth-v1", "fixture-key-v1");
        fs::remove_file(self.profile.join("secode.jsonc"))?;
        fs::remove_file(self.profile.join("chatgpt.oauth.json"))?;
        fs::write(self.profile.join("grapt.jsonc"), config)?;
        self.ready_store()?;
        let approval =
            grapt_platform::profile_configuration::ProfileConfiguration::capture(&self.profile)?
                .approval()
                .encoded()?;
        self.command_with_approval(approval)
    }

    fn install_plugin(&self) -> TestResult {
        let config = self.home.join(".config/secode");
        let plugin = config.join("claude-plugins/cache/retained");
        for (name, body) in self.legacy["plugin"]["files"]
            .as_object()
            .ok_or("plugin files missing")?
        {
            let path = plugin.join(name);
            fs::create_dir_all(path.parent().ok_or("plugin parent missing")?)?;
            fs::write(path, body.as_str().ok_or("plugin body missing")?)?;
        }
        let data = config.join("claude-plugins/data").join(
            self.legacy["plugin"]["data_digest"]
                .as_str()
                .ok_or("data digest missing")?,
        );
        fs::create_dir_all(&data)?;
        fs::write(data.join("retained.txt"), "existing plugin data")?;
        fs::write(
            config.join("secode.jsonc"),
            serde_json::to_vec(&json!({
                "claudePlugins":{"installed":{"legacy-plugin@fixture":{
                    "root":plugin,"digest":self.legacy["plugin"]["digest"],
                    "namespace":"legacy-plugin","bytes":self.legacy["plugin"]["bytes"],
                    "entry":{"name":"legacy-plugin"}}}}
            }))?,
        )?;
        Ok(())
    }

    fn seed_store(&self) -> TestResult {
        drop(self.open_store()?);
        // The old rows are inputs to the new store, never produced by its
        // current key/record encoders. This connection owns only fixture data.
        let store = rusqlite::Connection::open(self.profile.join("database.sqlite3"))?;
        let bytes = decode_hex(
            self.legacy["mail"]["record_hex"]
                .as_str()
                .ok_or("mail bytes missing")?,
        )?;
        store.execute(
            "INSERT INTO mcp_oauth_credential(key,revision,value) VALUES (?1,1,?2)",
            rusqlite::params![
                self.legacy["mcp_key"].as_str(),
                self.legacy["mcp_credential"].as_str()
            ],
        )?;
        store.execute("INSERT INTO mailbox_message(message_id,recipient,admission_seq,kind,supersedes,source_acknowledged,consumed,record_bytes) VALUES (?1,'fixture-recipient',1,'handoff',NULL,1,0,?2)",
            rusqlite::params![self.legacy["mail"]["local_id"].as_str(), bytes])?;
        Ok(())
    }

    fn open_store(&self) -> TestResult<EventStore> {
        Ok(EventStore::open(
            &self.profile.join("database.sqlite3"),
            "legacy-fixture",
        )?)
    }

    fn ready_store(&self) -> TestResult {
        drop(self.open_store()?);
        let migrations = grapt_store::ProductionMigrations::load()?;
        grapt_store::apply_migrations(
            &self.profile.join("database.sqlite3"),
            &self.profile,
            "0.1.0",
            &migrations.as_slice(),
        )?;
        Ok(())
    }
}

fn decode_hex(text: &str) -> TestResult<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return Err("odd fixture hex length".into());
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?))
        .collect()
}

fn mail_input() -> TestResult<RemoteMailboxInput> {
    Ok(RemoteMailboxInput {
        scope: RemoteMailboxScope {
            projects: ProjectPair::new(
                SourceProjectId::try_new("source-project")?,
                TargetProjectId::try_new("fixture-project")?,
            ),
            principal: SourcePrincipalId::try_new("fixture-recipient")?,
            recipient: SeatRecipientId::try_new("fixture-recipient")?,
        },
        protocol: ProtocolVersion::new("remote-seat-mail-transport/v1")
            .ok_or("protocol missing")?,
        source_message: RemoteMailboxMessageId::try_new("legacy-message")?,
        source_binding: SourceBindingId::try_new("legacy-binding")?,
        source_cursor: ExclusiveSourceCursor::try_new("legacy-cursor")?,
        sender: SourcePrincipalId::try_new("fixture-sender")?,
        kind: SeatMessageKind::Handoff,
        body: MailboxBody::new("Retained mail must not be ingested twice.")?,
        source_created: SourceTimestamp::try_new("2026-10-13T00:00:00.000Z")?,
        refs: Vec::new(),
        supersedes: None,
        superseded_by: None,
        archive: SourceArchiveDisposition::Active,
        attention: MailAttention::Wake,
    })
}

fn assert_retained_store(fixture: &Fixture) -> TestResult {
    let mut store = fixture.open_store()?;
    let key = McpCredentialKey::for_server("docs", "https://mcp.example.invalid/mcp")?;
    let token = store
        .mcp_credential(&key)?
        .value
        .ok_or("legacy MCP credential disappeared")?;
    let credential = grapt_mcp_client::oauth::tokens::OAuthCredential::decode(&token)?;
    credential.check_request("https://mcp.example.invalid/mcp", Some("fixture-client"))?;
    assert_eq!(
        credential.access_token().expose(),
        "SYNTHETIC_LEGACY_MCP_TOKEN_NOT_VALID"
    );
    let input = mail_input()?;
    let page = store.read_remote_mailbox(&input.scope, MailboxCursor::START, 16, 65536)?;
    assert_eq!(
        page.records.len(),
        1,
        "legacy mail became foreign or undecodable"
    );
    assert_eq!(
        page.records[0].message().body().as_str(),
        fixture.legacy["mail"]["body"]
    );
    let original = page.records[0].ack().clone();
    assert_eq!(
        store.admit_remote_mailbox(&input.scope, &input)?.ack,
        original
    );
    let count: i64 =
        store
            .connection()
            .query_row("SELECT count(*) FROM mailbox_message", [], |row| row.get(0))?;
    assert_eq!(count, 1, "reopening re-ingested existing mail");
    Ok(())
}

struct Running {
    child: Child,
    diagnostics: PathBuf,
}
impl Running {
    fn launch(mut command: Command, fixture: &Fixture) -> TestResult<(Self, Value)> {
        let diagnostics = fixture.profile.join("fixture-output.log");
        let log = fs::File::create(&diagnostics)?;
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?;
        let mut running = Self { child, diagnostics };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Ok(bytes) = fs::read(fixture.profile.join("serve-ready.json")) {
                if let Ok(ready) = serde_json::from_slice::<Value>(&bytes) {
                    if ready["pid"].as_u64() == Some(u64::from(running.child.id())) {
                        return Ok((running, ready));
                    }
                }
            }
            if let Some(status) = running.child.try_wait()? {
                return Err(format!(
                    "legacy profile launch refused ({status}): {}",
                    fs::read_to_string(&running.diagnostics)?
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(format!(
            "legacy profile readiness deadline: {}",
            fs::read_to_string(&running.diagnostics)?
        )
        .into())
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn get(address: SocketAddr, path: &str) -> TestResult<Value> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Basic Zml4dHVyZTpzeW50aGV0aWMtcGFzc3dvcmQ=\r\nConnection: close\r\n\r\n")?;
    let mut bytes = Vec::new();
    stream.take(262145).read_to_end(&mut bytes)?;
    assert!(bytes.len() <= 262144, "fixture response exceeded its bound");
    let split = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .ok_or("HTTP headers missing")?;
    let head = std::str::from_utf8(&bytes[..split])?;
    assert_eq!(
        head.split_whitespace().nth(1),
        Some("200"),
        "{path}: {head}"
    );
    let mut body = &bytes[split + 4..];
    let mut decoded = Vec::new();
    if head
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"))
    {
        loop {
            let at = body
                .windows(2)
                .position(|part| part == b"\r\n")
                .ok_or("chunk length missing")?;
            let length = usize::from_str_radix(std::str::from_utf8(&body[..at])?, 16)?;
            body = &body[at + 2..];
            if length == 0 {
                break;
            }
            decoded.extend_from_slice(body.get(..length).ok_or("chunk truncated")?);
            body = body.get(length + 2..).ok_or("chunk terminator missing")?;
        }
        body = &decoded;
    }
    Ok(serde_json::from_slice(body)?)
}

fn address(ready: &Value) -> TestResult<SocketAddr> {
    Ok(ready["observed_endpoint"]
        .as_str()
        .and_then(|s| s.strip_prefix("http://"))
        .ok_or("ready endpoint missing")?
        .parse()?)
}

#[test]
fn legacy_profile_opens_with_approval_credentials_plugin_and_mail_intact() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.install_plugin()?;
    fixture.seed_store()?;
    let tokens = chatgpt::load(&fixture.profile)?.ok_or("legacy ChatGPT envelope disappeared")?;
    assert_eq!(tokens.refresh, "SYNTHETIC_LEGACY_REFRESH_NOT_VALID");
    assert_retained_store(&fixture)?;
    fixture.ready_store()?;
    let (running, ready) = Running::launch(fixture.command()?, &fixture)?;
    assert_eq!(ready["schema"], "secode.product.ready.v2");
    let address = address(&ready)?;
    assert_eq!(
        get(address, "/api/health")?["schema"],
        "secode.api.health.v1"
    );
    let commands = get(address, "/api/command")?;
    assert!(
        commands.to_string().contains("legacy-plugin:retained"),
        "retained plugin failed to activate: {commands}"
    );
    drop(running);
    assert_retained_store(&fixture)?;
    assert!(fixture.profile.join("secode.jsonc").is_file());
    assert!(!fixture.profile.join("grapt.jsonc").exists());
    Ok(())
}

#[test]
fn retained_remote_mail_and_oauth_keys_survive_without_reingestion() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.seed_store()?;
    assert_retained_store(&fixture)
}

#[test]
fn legacy_approval_keeps_absent_configuration_name_framing() -> TestResult {
    let fixture = Fixture::new()?;
    let captured =
        grapt_platform::profile_configuration::ProfileConfiguration::capture(&fixture.profile)?;
    assert_eq!(captured.approval().encoded()?, fixture.approval()?);
    Ok(())
}

#[test]
fn legacy_plugin_tree_and_data_namespace_keep_their_fixed_digests() -> TestResult {
    let fixture = Fixture::new()?;
    let files = fixture.legacy["plugin"]["files"]
        .as_object()
        .ok_or("plugin files missing")?;
    let material: Vec<_> = files
        .iter()
        .map(|(name, body)| {
            Ok((
                name.as_str(),
                body.as_str().ok_or("plugin body missing")?.as_bytes(),
                false,
            ))
        })
        .collect::<TestResult<_>>()?;
    assert_eq!(
        grapt_plugin::claude::tree_digest(material.into_iter()),
        fixture.legacy["plugin"]["digest"]
    );
    assert_eq!(
        grapt_plugin::claude::tree_digest(std::iter::once((
            "legacy-plugin@fixture",
            b"".as_slice(),
            false
        ))),
        fixture.legacy["plugin"]["data_digest"]
    );
    Ok(())
}

#[tokio::test]
async fn sign_in_switch_writes_back_to_the_legacy_config() -> TestResult {
    let fixture = Fixture::new()?;
    let tokens = chatgpt::Tokens {
        access: "SYNTHETIC_NEW_ACCESS_NOT_VALID".into(),
        refresh: "SYNTHETIC_NEW_REFRESH_NOT_VALID".into(),
        account: "next-account".into(),
        expires_at: grapt_types::Timestamp::from_millis(4_000_000_000_000),
    };
    let replacement = b"{\"provider\":{\"account\":\"next-account\"}}";
    chatgpt::replace_sign_in::<(), chatgpt::SignInError>(&fixture.profile, &tokens, |_| {
        Ok(((), vec![chatgpt::SwitchFile::Config(replacement.to_vec())]))
    })
    .await?;
    assert_eq!(fs::read(fixture.profile.join("secode.jsonc"))?, replacement);
    assert!(!fixture.profile.join("grapt.jsonc").exists());
    assert_eq!(
        chatgpt::load(&fixture.profile)?
            .ok_or("switch lost sign-in")?
            .account,
        "next-account"
    );
    Ok(())
}

#[tokio::test]
async fn reconnect_reads_and_updates_the_legacy_profile_in_place() -> TestResult {
    let fixture = Fixture::new()?;
    let tokens = chatgpt::Tokens {
        access: "SYNTHETIC_NEW_ACCESS_NOT_VALID".into(),
        refresh: "SYNTHETIC_NEW_REFRESH_NOT_VALID".into(),
        account: "next-account".into(),
        expires_at: grapt_types::Timestamp::from_millis(4_000_000_000_000),
    };
    let listing = fs::read(fixture.profile.join("account-models.json"))?;
    let result = grapt_connections::chatgpt::resign(&fixture.profile, &tokens, &listing, 123)
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(result.previous_account, "fixture-account");
    let config: Value = serde_json::from_slice(&fs::read(fixture.profile.join("secode.jsonc"))?)?;
    assert_eq!(config["provider"]["account"], "next-account");
    assert!(!fixture.profile.join("grapt.jsonc").exists());
    Ok(())
}

#[test]
fn config_environment_prefers_current_then_legacy_and_ignores_foreign_selector() -> TestResult {
    for family in ["CONFIG", "CONFIG_CONTENT", "CONFIG_DIR"] {
        for (current, legacy, expected) in [
            (true, true, Some("current")),
            (false, true, Some("legacy")),
            (false, false, None),
        ] {
            let fixture = Fixture::new()?;
            let mut command = fixture.current_command()?;
            let mut inputs = Vec::new();
            for name in ["current", "legacy", "foreign"] {
                let content = json!({"commands":{(format!("selected-{name}")):{
                    "description":"Synthetic selection","template":"Synthetic selection"}}})
                .to_string();
                let file = fixture.home.join(format!("{name}.jsonc"));
                fs::write(&file, &content)?;
                let directory = fixture.home.join(name);
                fs::create_dir(&directory)?;
                fs::write(
                    directory.join(if name == "legacy" {
                        "secode.jsonc"
                    } else {
                        "grapt.jsonc"
                    }),
                    &content,
                )?;
                inputs.push((file, directory, content));
            }
            command.env(concat!("OPEN", "CODE_CONFIG"), &inputs[2].0);
            for (enabled, prefix, index) in [(current, "GRAPT", 0), (legacy, "SECODE", 1)] {
                if enabled {
                    let key = format!("{prefix}_{family}");
                    let (file, directory, content) = &inputs[index];
                    match family {
                        "CONFIG" => {
                            command.env(key, file);
                        }
                        "CONFIG_DIR" => {
                            command.env(key, directory);
                        }
                        _ => {
                            command.env(key, content);
                        }
                    }
                }
            }
            let (running, ready) = Running::launch(command, &fixture)?;
            let commands = get(address(&ready)?, "/api/command")?;
            let selected: Vec<_> = commands["data"]
                .as_array()
                .ok_or("command catalog missing")?
                .iter()
                .filter_map(|row| row["name"].as_str())
                .filter(|name| name.starts_with("selected-"))
                .collect();
            let expected: Vec<_> = expected
                .map(|name| format!("selected-{name}"))
                .into_iter()
                .collect();
            assert_eq!(
                selected, expected,
                "{family}: current={current}, legacy={legacy}"
            );
            drop(running);
        }
    }
    Ok(())
}

#[test]
fn legacy_connections_vault_and_discovery_open_without_rewriting() -> TestResult {
    use grapt_platform::profile_configuration::ProfileConfiguration;
    let fixture = Fixture::new()?;
    let retained: Value =
        serde_json::from_str(include_str!("rename_compatibility/connections.json"))?;
    for (name, field) in [
        ("connections.json", "connections"),
        ("connect-retained-a.credential", "vault"),
        ("connect-retained-a.json", "discovery"),
    ] {
        let bytes = retained[field]
            .as_str()
            .ok_or("retained connection bytes missing")?;
        let bytes = if field == "vault" {
            // Only platform protection is applied here; the vault document is
            // literal input from the pre-rename format, never a current encoder.
            profile_security::seal(bytes.as_bytes())?
        } else {
            bytes.as_bytes().to_vec()
        };
        profile_security::replace_private_file(&fixture.profile, name, &bytes)?;
    }
    let before: Vec<_> = [
        "connections.json",
        "connect-retained-a.credential",
        "connect-retained-a.json",
        "chatgpt.oauth.json",
        "provider.credential",
        "account-models.json",
        "secode.jsonc",
        "service.conf",
    ]
    .into_iter()
    .map(|name| Ok((name, fs::read(fixture.profile.join(name))?)))
    .collect::<TestResult<_>>()?;
    let approval = ProfileApproval::decode(
        &json!({"canonical":fixture.profile,
        "fingerprint":retained["approval_fingerprint"]})
        .to_string(),
    )?;
    assert_eq!(
        ProfileConfiguration::capture(&fixture.profile)?.approval(),
        &approval
    );
    assert_retained_vault(&fixture)?;
    fixture.ready_store()?;
    let (running, ready) = Running::launch(
        fixture.command_with_approval(approval.encoded()?)?,
        &fixture,
    )?;
    let address = address(&ready)?;
    let listed = get(address, "/api/integration")?;
    let connections = listed["data"]
        .as_array()
        .ok_or("integration catalog missing")?
        .iter()
        .flat_map(|row| row["connections"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    assert_eq!(connections.len(), 2, "retained catalog: {listed}");
    let status = get(address, "/api/auth/status")?;
    let rows = status["connections"]
        .as_array()
        .ok_or("authentication status missing")?;
    assert_eq!(rows.len(), 2);
    for id in ["openai", "work"] {
        let row = rows
            .iter()
            .find(|row| row["id"] == id)
            .ok_or("connection status missing")?;
        assert_eq!(row["status"], "ok", "{id}: {status}");
    }
    drop(running);
    for (name, bytes) in before {
        assert_eq!(
            fs::read(fixture.profile.join(name))?,
            bytes,
            "rewrote {name}"
        );
    }
    assert_eq!(
        ProfileConfiguration::capture(&fixture.profile)?.approval(),
        &approval
    );
    assert!(!fixture.profile.join("grapt.jsonc").exists());
    Ok(())
}

fn assert_retained_vault(fixture: &Fixture) -> TestResult {
    use grapt_login::vault::{CredentialVault, CredentialView, SealedFileVault, VaultKey};
    let records = grapt_connections::store::Store::read_snapshot(&fixture.profile)?
        .ok_or("retained connections disappeared")?;
    assert_eq!(records.connections.len(), 2);
    let work = records
        .connections
        .iter()
        .find(|row| row.id.as_str() == "work")
        .ok_or("retained key connection disappeared")?;
    let binding = work
        .credential_format
        .binding()
        .ok_or("vault binding disappeared")?;
    let vault = SealedFileVault::open(&fixture.profile)?;
    let key = VaultKey::new(work.id.clone(), &work.credential_file)?;
    let secret = vault
        .get(&key, binding)?
        .ok_or("retained key disappeared")?;
    match secret.expose() {
        CredentialView::ApiKey(value) => assert_eq!(value, "SYNTHETIC_RETAINED_KEY_NOT_VALID"),
        _ => return Err("retained API key changed credential kind".into()),
    }
    Ok(())
}

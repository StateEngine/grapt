//! Required-read interruption and reconnect use the actual registry and packet gate.
use super::*;
use grapt_types::{IdToken, InstructionKey};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const LOCATION: &str = "/seat-reconnect";
const CAPABILITY: &str = "RECONNECT_CAPABILITY";
const FIRST_CURSOR: &str = "RECONNECT_CURSOR_INITIAL";
const READY_CURSOR: &str = "RECONNECT_CURSOR_READY";
const READ_MAIL: &str = "Required inbox content from restored connection";

#[derive(Debug, Default)]
struct Ids(AtomicUsize);
impl IdAuthority for Ids {
    fn mint(&self) -> IdToken {
        IdToken::new(format!(
            "recon{:011x}",
            self.0.fetch_add(1, Ordering::SeqCst)
        ))
        .unwrap_or_else(|| panic!("fixture identifier"))
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Bootstrap,
    DropRequiredRead,
    Reconnected,
    WrongFingerprint,
    ColdCursor,
    ColdForeignPrincipal,
    ColdAgain,
    OtherRefusal,
    PartialCold,
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn peer(
    stream: tokio::io::DuplexStream,
    mode: Mode,
    calls: Arc<Mutex<Vec<Value>>>,
) -> Result<(), String> {
    let (read, mut write) = tokio::io::split(stream);
    let mut read = BufReader::new(read);
    let mut read_completed = false;
    for _ in 0..12 {
        let mut line = String::new();
        if tokio::io::AsyncReadExt::take(&mut read, 8192)
            .read_line(&mut line)
            .await
            .map_err(|error| error.to_string())?
            == 0
        {
            return Ok(());
        }
        let request: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        let result = match request["method"].as_str() {
            Some("initialize") => json!({"protocolVersion":grapt_mcp::PROTOCOL_VERSION,
                "capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
            Some("notifications/initialized") => continue,
            Some("tools/list") => json!({"tools":[
                {"name":"stateengine_resume","inputSchema":{"type":"object","properties":{
                    "since":{"type":"string"},"byte_budget":{"type":"integer"},"packet_id":{"type":"string"}}}},
                {"name":"stateengine_msg_list","inputSchema":{"type":"object"}}
            ]}),
            Some("tools/call") => {
                let params = &request["params"];
                assert_eq!(
                    params["_meta"]["stateengine.session-capability-v1"],
                    CAPABILITY
                );
                assert!(params["arguments"].get("agent_id").is_none());
                calls
                    .lock()
                    .map_err(|_| "fixture calls")?
                    .push(params.clone());
                let cold = matches!(
                    mode,
                    Mode::ColdCursor | Mode::ColdForeignPrincipal | Mode::ColdAgain
                );
                let has_cursor = params["arguments"].get("since").is_some();
                let refuse = params["name"] == "stateengine_resume"
                    && ((cold && has_cursor)
                        || matches!(mode, Mode::ColdAgain | Mode::OtherRefusal)
                        || matches!(mode, Mode::PartialCold) && read_completed);
                if refuse {
                    let reason = if matches!(mode, Mode::OtherRefusal) {
                        "temporary refusal"
                    } else {
                        "continuity cursor handle is absent; call stateengine_resume({}) with no since"
                    };
                    let result = json!({"isError":true,"content":[{"type":"text","text":reason}]});
                    write
                        .write_all(
                            (json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                                .to_string()
                                + "\n")
                                .as_bytes(),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    continue;
                }
                let body = match params["name"].as_str() {
                    Some("stateengine_msg_list") => {
                        if matches!(mode, Mode::DropRequiredRead) {
                            return Ok(());
                        }
                        assert_eq!(
                            params["arguments"],
                            json!({"unread_only":true,"select":"newest","limit":20})
                        );
                        read_completed = true;
                        json!({"complete":true,"messages":[{"body":READ_MAIL}]})
                    }
                    Some("stateengine_resume") => {
                        let reconnect =
                            matches!(mode, Mode::Reconnected | Mode::WrongFingerprint) || cold;
                        let pending = !reconnect && !read_completed;
                        let args = &params["arguments"];
                        if pending {
                            assert_eq!(args["packet_id"], "reconnect_packet");
                            assert!(args.get("since").is_none());
                        } else if cold {
                            assert!(args.get("since").is_none());
                            assert!(args.get("packet_id").is_none());
                        } else {
                            assert_eq!(
                                args["since"],
                                if reconnect {
                                    READY_CURSOR
                                } else {
                                    FIRST_CURSOR
                                }
                            );
                            assert!(
                                args.get("packet_id").is_none(),
                                "a reconnect must continue completed bootstrap"
                            );
                        }
                        let mut body = json!({"schema_version":2,"principal":{
                            "project_id":"fixture","agent_id":"agt_fixture","verified":true,
                            "identity_fingerprint":if matches!(mode,Mode::WrongFingerprint | Mode::ColdForeignPrincipal) {"foreign-fingerprint"} else {"fixture-fingerprint"}},
                            "resume_status":if pending {"needs_read"} else {"ready"},"complete":!pending,
                            "cursor":if pending {FIRST_CURSOR} else {READY_CURSOR},
                            "required_reads":if pending {vec![json!({"tool":"stateengine_msg_list","arguments":{
                                "unread_only":true,"select":"newest","limit":20}})]} else {vec![]},
                            "attention":{"task":if reconnect {"Restored current continuity"} else {"Initial continuity"}}});
                        if pending {
                            body["packet_acknowledgement"] = json!({"state":"acknowledged",
                                "packet_id":"reconnect_packet","recorded":true});
                        }
                        body
                    }
                    _ => return Err("unexpected fixture tool".into()),
                };
                json!({"content":[{"type":"text","text":body.to_string()}]})
            }
            _ => return Err("unexpected fixture method".into()),
        };
        write
            .write_all(
                (json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string() + "\n")
                    .as_bytes(),
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    Err("fixture request bound".into())
}

struct Fixture {
    packet: packet::tests::Fixture,
    inbox: Arc<Mutex<Inbox>>,
    custody: Arc<Mutex<SessionCapabilities>>,
    registry: Arc<McpRegistry>,
    session: SessionId,
    calls: Arc<Mutex<Vec<Value>>>,
    peers: Vec<tokio::task::JoinHandle<Result<(), String>>>,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let bytes = format!(
            "packet_id: reconnect_packet\nAdmitted fixture task. {CAPABILITY} {FIRST_CURSOR}\n"
        );
        let packet = packet::tests::Fixture::new(bytes.as_bytes())?;
        let session: SessionId = "ses_seat_reconnect".parse()?;
        let mut store = grapt_store::EventStore::open_in_memory()?;
        for (key, value) in [
            (
                "stateengine-identity",
                json!({"scope":"project","project_id":"fixture","agent_id":"agt_fixture"})
                    .to_string(),
            ),
            (
                "stateengine-packet",
                packet.pointer(bytes.as_bytes()).to_string(),
            ),
        ] {
            InstructionService::put_entry(
                &mut store,
                &session,
                &InstructionKey::new(key).ok_or("key")?,
                &value,
                1,
            )?;
        }
        let mut custody = SessionCapabilities::default();
        custody.put(session.clone(), CAPABILITY)?;
        Ok(Self {
            packet,
            session,
            inbox: Arc::new(Mutex::new(Inbox::new(store)?)),
            custody: Arc::new(Mutex::new(custody)),
            registry: Arc::new(super::super::mcp::registry(Arc::new(
                grapt_store::SystemClock,
            ))?),
            calls: Arc::new(Mutex::new(Vec::new())),
            peers: Vec::new(),
        })
    }

    async fn connect(&mut self, mode: Mode) -> ResumeContext {
        let (client, stream) = tokio::io::duplex(8192);
        self.peers
            .push(tokio::spawn(peer(stream, mode, self.calls.clone())));
        let declaration = super::super::mcp::Declaration::Local {
            command: vec!["fixture".into()],
            cwd: LOCATION.into(),
            environment: BTreeMap::new(),
            timeouts: super::super::mcp::McpTimeouts::default(),
        };
        self.registry
            .install_live_for_test(
                LOCATION,
                "stateengine",
                declaration.clone(),
                client,
                "fixture",
            )
            .await;
        let declarations = BTreeMap::from([("stateengine".into(), declaration)]);
        let published = self
            .registry
            .with_status(LOCATION, &declarations, |entries| {
                super::super::mcp::tool_definitions(entries, &[])
            })
            .await;
        ResumeContext::new(
            (
                self.session.clone(),
                AgentId::new("fixture").unwrap_or_else(|| panic!("agent")),
                LOCATION.into(),
            ),
            (
                self.inbox.clone(),
                self.custody.clone(),
                self.registry.clone(),
            ),
            Arc::new(published),
            Arc::new(Ids::default()),
        )
    }

    fn count(&self) -> Result<usize, Box<dyn std::error::Error>> {
        Ok(self.calls.lock().map_err(|_| "fixture calls")?.len())
    }

    async fn close(self) -> TestResult {
        drop(self.registry);
        for task in self.peers {
            tokio::time::timeout(Duration::from_secs(2), task).await???;
        }
        Ok(())
    }
}

fn text(resume: &mut ResumeContext) -> Result<String, Box<dyn std::error::Error>> {
    let CodexInputItem::Message(message) = resume.take_input()?.ok_or("missing continuity")? else {
        return Err("wrong context type".into());
    };
    assert!(!message.content.contains(CAPABILITY));
    assert!(!message.content.contains(FIRST_CURSOR));
    assert!(!message.content.contains(READY_CURSOR));
    assert_eq!(resume.take_input(), Err(ContinuityUnavailable::Incomplete));
    Ok(message.content)
}

#[tokio::test]
async fn required_inbox_disconnect_recovers_with_the_same_verified_packet() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut failed = fixture.connect(Mode::DropRequiredRead).await;
    assert_eq!(
        failed.refresh().await,
        Err(ContinuityUnavailable::Transport)
    );
    assert_eq!(failed.take_input(), Err(ContinuityUnavailable::Incomplete));
    assert!(fixture
        .custody
        .lock()
        .map_err(|_| "custody")?
        .resumes
        .is_empty());
    assert_eq!(fixture.count()?, 2);
    std::fs::remove_file(&fixture.packet.path)?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), failed.refresh()).await?,
        Err(ContinuityUnavailable::Transport),
        "an absent server must settle without looping or publishing partial context"
    );
    assert_eq!(failed.take_input(), Err(ContinuityUnavailable::Incomplete));
    assert_eq!(fixture.count()?, 2);
    drop(failed);
    let mut restored = fixture.connect(Mode::Bootstrap).await;
    restored.refresh().await?;
    let content = text(&mut restored)?;
    assert_eq!(content.matches(READ_MAIL).count(), 1);
    assert_eq!(content.matches("Admitted fixture task.").count(), 1);
    assert_eq!(fixture.count()?, 5);
    restored.refresh().await?;
    assert_eq!(text(&mut restored)?, content);
    assert_eq!(
        fixture.count()?,
        5,
        "completed bootstrap must not replay required reads"
    );
    drop(restored);
    fixture.close().await
}

#[tokio::test]
async fn reconnect_refreshes_completed_bootstrap_without_reack_or_private_handle_resurrection(
) -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut initial = fixture.connect(Mode::Bootstrap).await;
    initial.refresh().await?;
    assert!(text(&mut initial)?.contains(READ_MAIL));
    assert_eq!(fixture.count()?, 3);
    drop(initial);
    std::fs::remove_file(&fixture.packet.path)?;
    let mut reconnected = fixture.connect(Mode::Reconnected).await;
    reconnected.refresh().await?;
    let content = text(&mut reconnected)?;
    assert!(content.contains("Restored current continuity"));
    assert!(!content.contains("Initial continuity"));
    assert_eq!(fixture.count()?, 4);
    reconnected.refresh().await?;
    assert_eq!(text(&mut reconnected)?, content);
    assert_eq!(fixture.count()?, 4);
    drop(reconnected);
    let mut foreign = fixture.connect(Mode::WrongFingerprint).await;
    assert_eq!(
        foreign.refresh().await,
        Err(ContinuityUnavailable::Principal)
    );
    assert_eq!(foreign.take_input(), Err(ContinuityUnavailable::Incomplete));
    assert_eq!(fixture.count()?, 5);
    drop(foreign);
    fixture.close().await
}

#[test]
fn only_committed_non_incognito_identity_requires_continuity_startup() -> TestResult {
    let session: SessionId = "ses_startup_binding".parse()?;
    let unreadable = Inbox::new(grapt_store::EventStore::open_in_memory()?)?;
    assert!(matches!(
        is_required(&unreadable, &session),
        Err(ContinuityUnavailable::Binding)
    ));
    let mut initialized = grapt_store::EventStore::open_in_memory()?;
    InstructionService::put_entry(
        &mut initialized,
        &session,
        &InstructionKey::new("fixture-context").ok_or("context key")?,
        "An ordinary instruction without a StateEngine binding",
        1,
    )?;
    let empty = Inbox::new(initialized)?;
    assert!(!is_required(&empty, &session)?);
    for (identity, expected) in [
        (
            r#"{"scope":"project","project_id":"fixture","agent_id":"agt_fixture"}"#,
            Some(true),
        ),
        (r#"{"scope":"incognito"}"#, Some(false)),
        ("{", None),
    ] {
        let mut store = grapt_store::EventStore::open_in_memory()?;
        InstructionService::put_entry(
            &mut store,
            &session,
            &InstructionKey::new("stateengine-identity").ok_or("identity key")?,
            identity,
            1,
        )?;
        let inbox = Inbox::new(store)?;
        let actual = is_required(&inbox, &session);
        match expected {
            Some(expected) => assert_eq!(actual?, expected),
            None => assert!(matches!(actual, Err(ContinuityUnavailable::Binding))),
        }
    }
    Ok(())
}

#[tokio::test]
async fn lost_cursor_recovers_cold_without_reacknowledging_the_packet() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut initial = fixture.connect(Mode::Bootstrap).await;
    initial.refresh().await?;
    text(&mut initial)?;
    drop(initial);
    std::fs::remove_file(&fixture.packet.path)?;
    let mut restored = fixture.connect(Mode::ColdCursor).await;
    restored.refresh().await?;
    let content = text(&mut restored)?;
    assert!(content.contains("Restored current continuity"));
    assert!(!content.contains(READ_MAIL));
    assert!(!content.contains("Initial continuity"));
    assert_eq!(fixture.count()?, 5);
    restored.refresh().await?;
    assert_eq!(text(&mut restored)?, content);
    assert_eq!(fixture.count()?, 5);
    drop(restored);
    fixture.close().await
}

#[tokio::test]
async fn cold_resume_preserves_principal_and_refuses_repeated_or_other_errors() -> TestResult {
    for (mode, expected, count) in [
        (
            Mode::ColdForeignPrincipal,
            ContinuityUnavailable::Principal,
            5,
        ),
        (Mode::ColdAgain, ContinuityUnavailable::Transport, 5),
        (Mode::OtherRefusal, ContinuityUnavailable::Transport, 4),
    ] {
        let mut fixture = Fixture::new()?;
        let mut initial = fixture.connect(Mode::Bootstrap).await;
        initial.refresh().await?;
        text(&mut initial)?;
        drop(initial);
        let mut refused = fixture.connect(mode).await;
        assert_eq!(refused.refresh().await, Err(expected));
        assert_eq!(refused.take_input(), Err(ContinuityUnavailable::Incomplete));
        assert_eq!(fixture.count()?, count);
        drop(refused);
        fixture.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn lost_cursor_after_required_read_cannot_restart_partial_context() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut partial = fixture.connect(Mode::PartialCold).await;
    assert_eq!(
        partial.refresh().await,
        Err(ContinuityUnavailable::Transport)
    );
    assert_eq!(partial.take_input(), Err(ContinuityUnavailable::Incomplete));
    assert_eq!(fixture.count()?, 3);
    assert!(fixture
        .custody
        .lock()
        .map_err(|_| "custody")?
        .resumes
        .is_empty());
    drop(partial);
    fixture.close().await
}

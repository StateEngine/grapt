//! Packet handshakes exercise the actual registry and continuity publication gate.
use super::*;
use grapt_types::{IdToken, InstructionKey};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Default)]
struct Ids(AtomicUsize);
impl IdAuthority for Ids {
    fn mint(&self) -> IdToken {
        IdToken::new(format!("pkt{:012x}", self.0.fetch_add(1, Ordering::SeqCst)))
            .unwrap_or_else(|| panic!("fixture id invalid"))
    }
}

struct Peer {
    calls: Arc<AtomicUsize>,
    mode: &'static str,
    inbox: Arc<Mutex<Inbox>>,
    session: SessionId,
}

impl Peer {
    fn reply(&self, request: &Value) -> Result<Value, String> {
        let ordinal = self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            request["params"]["_meta"]["stateengine.session-capability-v1"],
            "PACKET_CAPABILITY"
        );
        assert_eq!(request["params"]["name"], "stateengine_resume");
        let args = &request["params"]["arguments"];
        // A fresh handshake after rebind remains healthy. A stale-packet
        // mutant must expose the reused bytes, not fail on a fixture rule.
        if self.mode.starts_with("identity-") && ordinal == 0 {
            assert_eq!(args, &json!({"byte_budget":MAX_REPLY}));
        } else if ordinal == 0 || (self.mode == "good" && ordinal >= 3) {
            assert_eq!(args["packet_id"], "packet_transport");
            assert!(args.get("since").is_none());
        } else {
            assert_eq!(
                args["since"],
                json!(format!("PACKET_CURSOR_{}", ordinal - 1))
            );
            assert!(args.get("packet_id").is_none());
        }
        assert!(args.get("agent_id").is_none());
        if self.mode == "pointer-rebind" {
            let mut inbox = self.inbox.lock().map_err(|_| "inbox")?;
            InstructionService::put_entry(
                inbox.store_mut(),
                &self.session,
                &InstructionKey::new("stateengine-packet").ok_or("key")?,
                "{}",
                2,
            )
            .map_err(|error| error.to_string())?;
        }
        let mut body = json!({"schema_version":2,
            "principal":{"project_id":"fixture","agent_id":if self.mode == "wrong-principal" {"foreign"} else {"agt_fixture"},"verified":true,"identity_fingerprint":if self.mode == "identity-principal-fingerprint" { "b".repeat(64) } else if self.mode.starts_with("identity-") { "a".repeat(64) } else { "packet-fingerprint".to_owned() }},
            "resume_status":"ready","complete":true,"required_reads":[],
            "cursor":format!("PACKET_CURSOR_{ordinal}"),"attention":{"task":"continue admitted packet task"}});
        if self.mode != "missing-ack" && !self.mode.starts_with("identity-") {
            body["packet_acknowledgement"] = json!({"state":"acknowledged",
                "packet_id":if self.mode == "wrong-ack" {"foreign_packet"} else {"packet_transport"},"recorded":true});
        }
        let mut content = vec![
            json!({"type":"text","text":body.to_string()}),
            json!({"type":"text","text":"stale_door: warning: bound door changed; PACKET_CAPABILITY PACKET_CURSOR_0"}),
        ];
        if self.mode == "ambiguous" {
            body["attention"]["task"] = json!("ambiguous-untrusted-task");
            content.push(json!({"type":"text","text":body.to_string()}));
        }
        Ok(json!({"content":content}))
    }
}

async fn server(stream: tokio::io::DuplexStream, peer: Peer) -> Result<(), String> {
    let (read, mut write) = tokio::io::split(stream);
    let mut read = BufReader::new(read);
    for _ in 0..8 {
        let mut line = String::new();
        let count = tokio::io::AsyncReadExt::take(&mut read, 8192)
            .read_line(&mut line)
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Ok(());
        }
        let request: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        let result = match request["method"].as_str() {
            Some("initialize") => json!({"protocolVersion":grapt_mcp::PROTOCOL_VERSION,
                "capabilities":{"tools":{}},"serverInfo":{"name":"stateengine","version":"packet-fixture"}}),
            Some("notifications/initialized") => continue,
            Some("tools/list") => {
                let mut schema = json!({"type":"object","properties":{"since":{"type":"string"},"byte_budget":{"type":"integer"}}});
                if !peer.mode.starts_with("identity-") {
                    schema["properties"]["packet_id"] = json!({"type":"string"});
                }
                json!({"tools":[{"name":"stateengine_resume","inputSchema":schema}]})
            }
            Some("tools/call") => peer.reply(&request)?,
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
    Err("fixture call bound reached".into())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn packet_resume_requires_ack_and_reuses_verified_bytes_after_compaction(
) -> Result<(), Box<dyn std::error::Error>> {
    for mode in [
        "good",
        "missing-ack",
        "wrong-ack",
        "wrong-principal",
        "pointer-rebind",
        "corrupt",
        "ambiguous",
        "identity-good",
        "identity-malformed-fingerprint",
        "identity-agent-mismatch",
        "identity-project-mismatch",
        "identity-principal-fingerprint",
        "identity-duplicate-line",
        "identity-hash-mismatch",
    ] {
        let body = if mode.starts_with("identity-") {
            format!("agent_id: {}\nproject_id: {}\nidentity_fingerprint: {}\nCall stateengine_resume({{}}) before work.\n{}",
                if mode == "identity-agent-mismatch" {"foreign-agent"} else {"agt_fixture"},
                if mode == "identity-project-mismatch" {"foreign-project"} else {"fixture"},
                if mode == "identity-malformed-fingerprint" {"A".repeat(64)} else {"a".repeat(64)},
                if mode == "identity-duplicate-line" {"agent_id: agt_fixture\n"} else {""})
        } else {
            "packet_id: packet_transport\nAdmitted launch task. PACKET_CAPABILITY PACKET_CURSOR_0\n"
                .to_owned()
        };
        let bytes = body.as_bytes();
        let fixture = packet::tests::Fixture::new(bytes)?;
        let pointer = fixture.pointer(bytes).to_string();
        if matches!(mode, "corrupt" | "identity-hash-mismatch") {
            let mut corrupt = bytes.to_vec();
            corrupt[bytes.len() - 2] = b'!';
            std::fs::write(&fixture.path, corrupt)?;
        }
        let mut store = grapt_store::EventStore::open_in_memory()?;
        let session: SessionId = "ses_packet_transport".parse()?;
        for (key, value) in [
            (
                "stateengine-identity",
                r#"{"scope":"project","project_id":"fixture","agent_id":"agt_fixture"}"#,
            ),
            ("stateengine-packet", pointer.as_str()),
        ] {
            InstructionService::put_entry(
                &mut store,
                &session,
                &InstructionKey::new(key).ok_or("key")?,
                value,
                1,
            )?;
        }
        let inbox = Arc::new(Mutex::new(Inbox::new(store)?));
        let custody = Arc::new(Mutex::new(SessionCapabilities::default()));
        custody
            .lock()
            .map_err(|_| "custody")?
            .put(session.clone(), "PACKET_CAPABILITY")?;
        let registry = Arc::new(super::super::mcp::registry(std::sync::Arc::new(
            grapt_store::SystemClock,
        ))?);
        let calls = Arc::new(AtomicUsize::new(0));
        let (client, stream) = tokio::io::duplex(8192);
        let peer = tokio::spawn(server(
            stream,
            Peer {
                calls: Arc::clone(&calls),
                mode,
                inbox: Arc::clone(&inbox),
                session: session.clone(),
            },
        ));
        let directory = "/packet-resume";
        let declaration = super::super::mcp::Declaration::Local {
            command: vec!["fixture".into()],
            cwd: directory.into(),
            environment: BTreeMap::new(),
            timeouts: super::super::mcp::McpTimeouts::default(),
        };
        registry
            .install_live_for_test(
                directory,
                "stateengine",
                declaration.clone(),
                client,
                "fixture",
            )
            .await;
        let declared = BTreeMap::from([("stateengine".to_owned(), declaration)]);
        let published = Arc::new(
            registry
                .with_status(directory, &declared, |entries| {
                    super::super::mcp::tool_definitions(entries, &[])
                })
                .await,
        );
        let mut resume = ResumeContext::new(
            (
                session.clone(),
                AgentId::new("fixture").ok_or("agent")?,
                directory.into(),
            ),
            (
                Arc::clone(&inbox),
                Arc::clone(&custody),
                Arc::clone(&registry),
            ),
            published,
            Arc::new(Ids::default()),
        );
        if matches!(mode, "good" | "identity-good") {
            resume.refresh().await?;
            let CodexInputItem::Message(first) = resume.take_input()?.ok_or("context")? else {
                panic!("message");
            };
            if mode == "good" {
                assert!(first.content.contains("Admitted launch task."));
            } else {
                assert!(first.content.contains("continue admitted packet task"));
                assert!(!first.content.contains("launch_packet"));
            }
            assert!(first.content.contains("stale_door:"));
            assert!(!first.content.contains("PACKET_CAPABILITY"));
            assert!(!first.content.contains("PACKET_CURSOR"));
            std::fs::remove_file(&fixture.path)?;
            resume.refresh().await?;
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            for (generation, expected_calls) in [(1_u64, 2_usize), (2, 3)] {
                super::transport_tests::commit_window(&inbox, &session, generation)?;
                resume.refresh().await?;
                assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
                let CodexInputItem::Message(delta) = resume.take_input()?.ok_or("context")? else {
                    panic!("message");
                };
                if mode == "good" {
                    assert!(delta.content.contains("Admitted launch task."));
                } else {
                    assert!(!delta.content.contains("launch_packet"));
                }
                assert!(!delta.content.contains("PACKET_CAPABILITY"));
                assert!(
                    !delta.content.contains("PACKET_CURSOR"),
                    "an older packet cursor must not resurrect after another compaction"
                );
            }
            custody
                .lock()
                .map_err(|_| "custody")?
                .put(session.clone(), "PACKET_CAPABILITY")?;
            assert_eq!(
                resume.refresh().await,
                Err(ContinuityUnavailable::PacketMissing),
                "rebind must require a newly delivered packet"
            );
        } else {
            let expected = match mode {
                "wrong-principal" => ContinuityUnavailable::Principal,
                "pointer-rebind" => ContinuityUnavailable::Rebound,
                "corrupt"
                | "identity-hash-mismatch"
                | "identity-malformed-fingerprint"
                | "identity-duplicate-line" => ContinuityUnavailable::PacketInvalid,
                "identity-agent-mismatch"
                | "identity-project-mismatch"
                | "identity-principal-fingerprint" => ContinuityUnavailable::Principal,
                "ambiguous" => ContinuityUnavailable::Contract,
                _ => ContinuityUnavailable::PacketAcknowledgement,
            };
            assert_eq!(resume.refresh().await, Err(expected));
            assert_eq!(resume.take_input(), Err(ContinuityUnavailable::Incomplete));
            assert!(custody.lock().map_err(|_| "custody")?.resumes.is_empty());
            assert_eq!(
                calls.load(Ordering::SeqCst),
                usize::from(!matches!(
                    mode,
                    "corrupt"
                        | "identity-hash-mismatch"
                        | "identity-malformed-fingerprint"
                        | "identity-duplicate-line"
                        | "identity-agent-mismatch"
                        | "identity-project-mismatch"
                )),
                "{mode}"
            );
        }
        drop(resume);
        drop(registry);
        tokio::time::timeout(Duration::from_secs(2), peer).await???;
    }
    Ok(())
}

use super::*;
use grapt_types::{IdToken, InstructionKey};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Default)]
struct Ids(AtomicUsize);
impl IdAuthority for Ids {
    fn mint(&self) -> IdToken {
        IdToken::new(format!("scc{:012x}", self.0.fetch_add(1, Ordering::SeqCst)))
            .unwrap_or_else(|| panic!("fixture id invalid"))
    }
}

pub(super) fn commit_window(
    inbox: &Mutex<Inbox>,
    session: &SessionId,
    generation: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_types::{
        ArtifactDigest, ArtifactId, ArtifactRef, CompactionDescriptor, CompactionDescriptorParts,
        CompactionId, CompactionSummary, CompactionTrigger, ContextEpoch, NextRequestRef,
        RetainedHistory, RetentionPolicyVersion, StepNumber,
    };
    let artifact = ArtifactRef::new(
        ArtifactId::new("scc-resume-reference").ok_or("artifact")?,
        12,
        ArtifactDigest::new("fixture-reference").ok_or("digest")?,
    );
    let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
        id: CompactionId::new(format!("scc-resume-compaction-{generation}")).ok_or("compaction")?,
        source_session: session.clone(),
        source_epoch: ContextEpoch::new(generation.checked_sub(1).ok_or("generation")?),
        target_window: ContextWindowId::new(format!("scc-resume-window-{generation}"))
            .ok_or("window")?,
        target_epoch: ContextEpoch::new(generation),
        trigger: CompactionTrigger::ContextOverflow,
        retention_policy: RetentionPolicyVersion::new("fixture").ok_or("policy")?,
        summary: CompactionSummary::ClientReadable {
            artifact: artifact.clone(),
        },
        retained: RetainedHistory::DeterministicSelection {
            descriptor: artifact.clone(),
        },
        baseline: artifact.clone(),
        next_request: NextRequestRef::new(StepNumber::new(1).ok_or("step")?, artifact),
    })
    .map_err(|e| format!("{e:?}"))?;
    let mut inbox = inbox.lock().map_err(|_| "inbox")?;
    inbox
        .store_mut()
        .elect_writer(&grapt_store::AggregateId::Session(session.clone()))?;
    let writer = inbox
        .store()
        .fencing_token(&grapt_store::AggregateId::Session(session.clone()))?;
    let tx = inbox.store_mut().governed_transaction()?;
    grapt_store::compaction::record(
        &tx,
        &format!("evt_scc_resume_window_{generation}").parse()?,
        writer,
        2,
        grapt_store::compaction::CompactionAttempt::Settled(&descriptor),
        &mut [],
    )?;
    tx.commit()?.discard();
    Ok(())
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn server(
    stream: tokio::io::DuplexStream,
    calls: Arc<AtomicUsize>,
    mode: String,
    resume_delay: Duration,
    shout: Option<String>,
    rebind: Option<(Arc<Mutex<SessionCapabilities>>, SessionId)>,
) -> Result<(), String> {
    let wrong = mode == "wrong";
    let incomplete = mode == "incomplete";
    let v2_reads = mode == "v2-omitted-reads";
    let rust = mode.starts_with("rust-");
    let (read, mut write) = tokio::io::split(stream);
    let mut read = BufReader::new(read);
    for _ in 0..12 {
        let mut line = String::new();
        let count = tokio::io::AsyncReadExt::take(&mut read, 8192)
            .read_line(&mut line)
            .await
            .map_err(|e| e.to_string())?;
        if count == 0 {
            return Ok(());
        }
        let request: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        let result = match request["method"].as_str() {
            Some("initialize") => {
                json!({"protocolVersion":grapt_mcp::PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"stateengine","version":"fixture-2"}})
            }
            Some("notifications/initialized") => continue,
            Some("tools/list") if rust => json!({"tools":[{
                "name":"stateengine_resume","inputSchema":{
                    "$defs":{"Cursor":{"type":"string","minLength":1,"maxLength":4096}},
                    "type":"object","properties":{
                        "since":{"anyOf":[{"$ref":"#/$defs/Cursor"},{"type":"null"}],"default":null},
                        "byte_budget":{"type":"integer"}}}
            }]}),
            Some("tools/list") => json!({"tools":[
                {"name":"stateengine_resume","inputSchema":{"type":"object","properties":{"since":{"type":"string"},"byte_budget":{"type":"integer"}}}},
                {"name":"stateengine_msg_list","inputSchema":{"type":"object"}}
            ]}),
            Some("tools/call") => {
                let ordinal = calls.fetch_add(1, Ordering::SeqCst);
                if ordinal == 0 {
                    tokio::time::sleep(resume_delay).await;
                    if let Some((custody, session)) = &rebind {
                        custody
                            .lock()
                            .map_err(|_| "custody")?
                            .put(session.clone(), "SCC_CAPABILITY")?;
                    }
                }
                assert_eq!(
                    request["params"]["_meta"]["stateengine.session-capability-v1"],
                    "SCC_CAPABILITY"
                );
                assert!(request["params"]["arguments"].get("agent_id").is_none());
                let body = if rust {
                    assert_eq!(request["params"]["name"], "stateengine_resume");
                    if ordinal == 0 {
                        assert!(request["params"]["arguments"].get("since").is_none());
                    } else {
                        assert_eq!(
                            request["params"]["arguments"]["since"],
                            rust_cursor(&mode, 0)
                        );
                    }
                    rust_reply(&mode, ordinal)
                } else if ordinal == 1 && !wrong {
                    assert_eq!(request["params"]["name"], "stateengine_msg_list");
                    assert_eq!(
                        request["params"]["arguments"],
                        json!({"unread_only":true,"select":"newest","limit":20})
                    );
                    json!({"complete":true,"messages":[{"body":"finish the admitted fixture task"}]})
                } else {
                    assert_eq!(request["params"]["name"], "stateengine_resume");
                    if ordinal == 0 || ordinal == 4 {
                        assert!(request["params"]["arguments"].get("since").is_none());
                    }
                    if ordinal == 2 {
                        assert_eq!(request["params"]["arguments"]["since"], "SCC_CURSOR_0");
                    }
                    if ordinal == 3 {
                        assert_eq!(request["params"]["arguments"]["since"], "SCC_CURSOR_2");
                    }
                    let pending = ordinal == 0 && !wrong;
                    let mut body = json!({"schema_version":2,"principal":{"project_id":"fixture","agent_id":if wrong {"agt_foreign"} else {"agt_fixture"},"verified":true,"identity_fingerprint":"fixture-fingerprint"},
                        "resume_status":if pending {"needs_read"} else {"ready"},"complete":!incomplete,
                        "cursor":format!("SCC_CURSOR_{ordinal}"),
                        "required_reads":if pending {vec![json!({"tool":"stateengine_msg_list","arguments":{"unread_only":true,"select":"newest","limit":20}})]} else {vec![]},
                        "attention":{"status":"has_admitted_work","task":"continue fixture coding", "previous_private_handle":if ordinal == 2 {"SCC_CURSOR_0"} else {"none"}}});
                    if v2_reads && !pending {
                        if let Some(object) = body.as_object_mut() {
                            object.remove("required_reads");
                        }
                    }
                    body
                };
                let mut result = json!({"content":[{"type":"text","text":body.to_string()}]});
                if mode == "rust-error" {
                    result["isError"] = json!(true);
                }
                if ordinal == 0 {
                    if let Some(shout) = &shout {
                        result["content"]
                            .as_array_mut()
                            .ok_or("fixture content array")?
                            .push(json!({"type":"text","text":shout}));
                    }
                }
                result
            }
            _ => return Err("unexpected fixture method".into()),
        };
        write
            .write_all(
                (json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string() + "\n")
                    .as_bytes(),
            )
            .await
            .map_err(|e| e.to_string())?;
    }
    Err("fixture call bound reached".into())
}

fn rust_cursor(mode: &str, ordinal: usize) -> String {
    if mode == "rust-position" && ordinal == 0 {
        format!("Exact.Position~{}", "x".repeat(1400))
    } else {
        format!("SCC_CURSOR_{ordinal}")
    }
}

fn rust_reply(mode: &str, ordinal: usize) -> Value {
    let mut reply = json!({
        "schema":"se.resume/v1","schema_version":1,
        "principal":{"project":{"id":"fixture","name":"fixture project"},
            "agent":{"id":"agt_fixture","name":"fixture agent"},"preview":false,
            "session_id":if mode == "rust-principal-change" && ordinal > 0 {
                "ses_other_remote"
            } else {"ses_remote"}},
        "cursor":rust_cursor(mode, ordinal),"since":null,
        "resume_status":"ready","complete":false,
        "code_status":{"state":"Unreadable","complete":false},
        "working_state":{"state":"absent"},"last_compaction":{"state":"absent"},
        "transcript_events":{"state":"present","value":[]},
        "required_reads":[],"omissions":[],
        "unread":{"items":[],"window":{"complete":true,"omitted":{"count":0,"reason":null}}}
    });
    if mode == "rust-needs-read" {
        reply["resume_status"] = json!("needs_read");
        reply["required_reads"] = json!([{
            "kind":"message","id":"msg_required","why":"read the complete authenticated message"
        }]);
        reply["omissions"] = json!([
            {"section":"recent_turns","reason":"byte budget","tool":"stateengine_event_read",
                "command":"","argv":[]},
            {"section":"working_state","reason":"byte budget","tool":"stateengine_working_state_get",
                "command":"","argv":[]},
            {"section":"recovery_commands","reason":"byte budget; expand resume for commands",
                "tool":"stateengine_resume","command":"se resume --budget 1048576",
                "argv":["se","resume","--budget","1048576"]}
        ]);
    } else if mode == "rust-integrity-blocked" {
        reply["resume_status"] = json!("integrity_blocked");
    } else if mode == "rust-unreadable" {
        reply["transcript_events"] = json!({"state":"unreadable"});
    } else if mode == "rust-wrong-project" {
        reply["principal"]["project"]["id"] = json!("foreign");
    } else if mode == "rust-wrong-agent" {
        reply["principal"]["agent"]["id"] = json!("agt_foreign");
    } else if mode == "rust-preview" {
        reply["principal"]["preview"] = json!(true);
    } else if mode == "rust-missing-session" {
        if let Some(principal) = reply["principal"].as_object_mut() {
            principal.remove("session_id");
        }
    } else if mode == "rust-null-session" {
        reply["principal"]["session_id"] = Value::Null;
    } else if mode == "rust-false-ready" {
        if let Some(reply) = reply.as_object_mut() {
            reply.remove("code_status");
        }
    } else if mode == "rust-unreadable-message" {
        reply["resume_status"] = json!("needs_read");
        reply["required_reads"] =
            json!([{"kind":"message","id":"msg_required","why":"read the complete message"}]);
        reply["unread"]["items"] =
            json!([{"id":"msg_required","unreadable":{"reason":"corrupt source"}}]);
    }
    reply
}

#[tokio::test]
async fn rust_continuity_reaches_the_consumer_without_running_recovery_commands(
) -> Result<(), Box<dyn std::error::Error>> {
    exercise(&[
        "rust-ready",
        "rust-needs-read",
        "rust-integrity-blocked",
        "rust-unreadable",
        "rust-principal-change",
        "rust-position",
        "rust-wrong-project",
        "rust-wrong-agent",
        "rust-preview",
        "rust-missing-session",
        "rust-null-session",
        "rust-false-ready",
        "rust-unreadable-message",
        "rust-error",
    ])
    .await
}

#[tokio::test]
async fn scc_resume_uses_real_registry_required_reads_once_and_rebind_clears_cursor(
) -> Result<(), Box<dyn std::error::Error>> {
    exercise(&[
        "good",
        "wrong",
        "rebind-during-io",
        "incomplete",
        "incognito",
        "v2-omitted-reads",
    ])
    .await
}

#[tokio::test]
async fn resume_after_legacy_five_second_budget_is_admitted(
) -> Result<(), Box<dyn std::error::Error>> {
    exercise(&["slow"]).await
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn exercise(modes: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    for &mode in modes {
        let wrong = mode == "wrong";
        let mut store = grapt_store::EventStore::open_in_memory()?;
        let session: SessionId = "ses_scc_resume".parse()?;
        let key = InstructionKey::new("stateengine-identity").ok_or("key")?;
        InstructionService::put_entry(
            &mut store,
            &session,
            &key,
            if mode == "incognito" {
                r#"{"scope":"incognito"}"#
            } else {
                r#"{"scope":"project","project_id":"fixture","agent_id":"agt_fixture"}"#
            },
            1,
        )?;
        let inbox = Arc::new(Mutex::new(Inbox::new(store)?));
        let custody = Arc::new(Mutex::new(SessionCapabilities::default()));
        custody
            .lock()
            .map_err(|_| "custody")?
            .put(session.clone(), "SCC_CAPABILITY")?;
        let registry = Arc::new(super::super::mcp::registry(std::sync::Arc::new(
            grapt_store::SystemClock,
        ))?);
        let calls = Arc::new(AtomicUsize::new(0));
        let (client, peer) = tokio::io::duplex(8192);
        let rebind = (mode == "rebind-during-io").then(|| (Arc::clone(&custody), session.clone()));
        let peer = tokio::spawn(server(
            peer,
            Arc::clone(&calls),
            mode.to_owned(),
            if mode == "slow" {
                Duration::from_secs(6)
            } else {
                Duration::ZERO
            },
            if mode.starts_with("shout-") {
                let state = if mode.contains("unreadable") {
                    "unreadable"
                } else {
                    "behind"
                };
                let refusal = mode.ends_with("refusal");
                let fixture = super::reply::real_shouts()
                    .into_iter()
                    .find(|fixture| {
                        fixture["state"] == state
                            && fixture["answer"].get("refusal").is_some() == refusal
                    })
                    .ok_or("real shout variant is absent")?;
                Some(
                    fixture["text"]
                        .as_str()
                        .ok_or("real shout text")?
                        .to_owned(),
                )
            } else {
                None
            },
            rebind,
        ));
        let directory = "/scc-resume";
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
        assert_eq!(resume.take_input(), Err(ContinuityUnavailable::Incomplete));
        if mode.starts_with("rust-") {
            let refused = !matches!(
                mode,
                "rust-ready" | "rust-needs-read" | "rust-principal-change" | "rust-position"
            );
            let result = resume.refresh().await;
            assert_eq!(calls.load(Ordering::SeqCst), 1, "automatic recovery call");
            if refused {
                assert!(result.is_err(), "{mode}: required continuity must refuse");
                assert_eq!(resume.take_input(), Err(ContinuityUnavailable::Incomplete));
                assert!(custody.lock().map_err(|_| "custody")?.resumes.is_empty());
            } else {
                result.map_err(|error| error.to_string())?;
                let Some(CodexInputItem::Message(message)) =
                    resume.take_input().map_err(|error| error.to_string())?
                else {
                    return Err("Rust continuity did not reach model input".into());
                };
                assert!(!message.content.contains("SCC_CAPABILITY"));
                let admitted: Value =
                    serde_json::from_str(message.content.lines().last().ok_or("context JSON")?)?;
                let expected = rust_reply(mode, 0);
                for key in [
                    "principal",
                    "complete",
                    "resume_status",
                    "required_reads",
                    "omissions",
                    "code_status",
                ] {
                    assert_eq!(admitted.get(key), expected.get(key), "{mode}: {key}");
                }
                assert!(message.content.contains("\"complete\":false"));
                if mode == "rust-needs-read" {
                    assert!(message.content.contains("msg_required"));
                    assert!(message.content.contains("se resume --budget 1048576"));
                    assert!(message.content.contains("\"needs_read\""));
                }
                resume.refresh().await.map_err(|error| error.to_string())?;
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                if mode == "rust-principal-change" {
                    commit_window(&inbox, &session, 1)?;
                    assert_eq!(
                        resume.refresh().await,
                        Err(ContinuityUnavailable::Principal)
                    );
                    assert_eq!(calls.load(Ordering::SeqCst), 2);
                    assert_eq!(resume.take_input(), Err(ContinuityUnavailable::Incomplete));
                } else if mode == "rust-position" {
                    commit_window(&inbox, &session, 1)?;
                    resume.refresh().await.map_err(|error| error.to_string())?;
                    let Some(CodexInputItem::Message(message)) = resume.take_input()? else {
                        return Err("refreshed Rust continuity absent".into());
                    };
                    let refreshed: Value = serde_json::from_str(
                        message.content.lines().last().ok_or("context JSON")?,
                    )?;
                    assert_eq!(refreshed["principal"], admitted["principal"]);
                    assert_eq!(calls.load(Ordering::SeqCst), 2);
                }
            }
        } else if mode == "incognito" {
            resume.refresh().await.map_err(|e| e.to_string())?;
            assert_eq!(resume.take_input(), Ok(None));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(custody.lock().map_err(|_| "custody")?.resumes.is_empty());
        } else if mode == "incomplete" {
            assert_eq!(
                resume.refresh().await,
                Err(ContinuityUnavailable::Incomplete)
            );
            assert_eq!(calls.load(Ordering::SeqCst), 3);
            assert!(custody.lock().map_err(|_| "custody")?.resumes.is_empty());
            assert_eq!(resume.take_input(), Err(ContinuityUnavailable::Incomplete));
        } else if mode == "rebind-during-io" {
            assert_eq!(resume.refresh().await, Err(ContinuityUnavailable::Rebound));
            assert_eq!(calls.load(Ordering::SeqCst), 3);
            assert!(custody.lock().map_err(|_| "custody")?.resumes.is_empty());
            assert_eq!(resume.take_input(), Err(ContinuityUnavailable::Incomplete));
        } else if wrong {
            assert_eq!(
                resume.refresh().await,
                Err(ContinuityUnavailable::Principal)
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(custody.lock().map_err(|_| "custody")?.resumes.is_empty());
        } else {
            resume.refresh().await.map_err(|e| e.to_string())?;
            assert_eq!(calls.load(Ordering::SeqCst), 3);
            let first = resume
                .take_input()
                .map_err(|e| e.to_string())?
                .ok_or("context")?;
            let CodexInputItem::Message(first) = first else {
                panic!("message")
            };
            assert!(first.content.contains("finish the admitted fixture task"));
            assert!(!first.content.contains("SCC_CURSOR"));
            assert!(!first.content.contains("SCC_CAPABILITY"));
            if mode.starts_with("shout-") {
                assert!(
                    !first.content.contains("this MCP door"),
                    "raw shout entered model context"
                );
            }
            resume.refresh().await.map_err(|e| e.to_string())?;
            assert_eq!(
                calls.load(Ordering::SeqCst),
                3,
                "ordinary request must reuse completed continuity"
            );
            commit_window(&inbox, &session, 1)?;
            resume.refresh().await.map_err(|e| e.to_string())?;
            assert_eq!(
                calls.load(Ordering::SeqCst),
                4,
                "new context window requires delta resume"
            );
            resume.refresh().await.map_err(|e| e.to_string())?;
            assert_eq!(
                calls.load(Ordering::SeqCst),
                4,
                "completed delta is reused in its window"
            );
            custody
                .lock()
                .map_err(|_| "custody")?
                .put(session.clone(), "SCC_CAPABILITY")?;
            resume.refresh().await.map_err(|e| e.to_string())?;
            assert_eq!(
                calls.load(Ordering::SeqCst),
                5,
                "explicit rebind requires cold resume"
            );
            *custody.lock().map_err(|_| "custody")? = SessionCapabilities::default();
            assert_eq!(
                resume.refresh().await,
                Err(ContinuityUnavailable::Capability)
            );
            assert_eq!(calls.load(Ordering::SeqCst), 5);
        }
        drop(resume);
        drop(registry);
        tokio::time::timeout(Duration::from_secs(2), peer).await???;
    }
    Ok(())
}

#[tokio::test]
async fn seat05_real_door_shouts_survive_the_ordinary_resume_path(
) -> Result<(), Box<dyn std::error::Error>> {
    exercise(&[
        "shout-behind",
        "shout-behind-refusal",
        "shout-unreadable",
        "shout-unreadable-refusal",
    ])
    .await
}

#[tokio::test(start_paused = true)]
async fn a_stalled_resume_returns_before_the_step_watchdog(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = grapt_store::EventStore::open_in_memory()?;
    let session: SessionId = "ses_deadline_fixture".parse()?;
    InstructionService::put_entry(
        &mut store,
        &session,
        &InstructionKey::new("stateengine-identity").ok_or("key")?,
        r#"{"scope":"project","project_id":"fixture","agent_id":"agt_fixture"}"#,
        1,
    )?;
    let inbox = Arc::new(Mutex::new(Inbox::new(store)?));
    let custody = Arc::new(Mutex::new(SessionCapabilities::default()));
    custody
        .lock()
        .map_err(|_| "custody")?
        .put(session.clone(), "SCC_CAPABILITY")?;
    let registry = Arc::new(crate::dogfood::mcp::registry(Arc::new(
        grapt_store::SystemClock,
    ))?);
    let calls = Arc::new(AtomicUsize::new(0));
    let (client, peer) = tokio::io::duplex(8192);
    let peer = tokio::spawn(server(
        peer,
        calls.clone(),
        "good".into(),
        Duration::from_secs(3600),
        None,
        None,
    ));
    let directory = "/deadline-fixture";
    let declaration = crate::dogfood::mcp::Declaration::Local {
        command: vec!["fixture".into()],
        cwd: directory.into(),
        environment: BTreeMap::new(),
        timeouts: crate::dogfood::mcp::McpTimeouts::default(),
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
                crate::dogfood::mcp::tool_definitions(entries, &[])
            })
            .await,
    );
    let mut resume = ResumeContext::new(
        (
            session,
            AgentId::new("fixture").ok_or("agent")?,
            directory.into(),
        ),
        (inbox, custody, registry.clone()),
        published,
        Arc::new(Ids::default()),
    );
    let watchdog = grapt_runtime::StepPolicy::ruled_default()?.no_progress_timeout();
    let before = tokio::time::Instant::now();
    let result = tokio::time::timeout(watchdog - Duration::from_secs(1), resume.refresh()).await;
    let elapsed = before.elapsed();
    drop(resume);
    drop(registry);
    peer.abort();
    let closed = peer.await;
    assert!(closed.is_err_and(|error| error.is_cancelled()));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the real MCP call must have started"
    );
    assert!(
        matches!(result, Ok(Err(ContinuityUnavailable::TimedOut { .. }))),
        "the Step watchdog would win over the sidecar: {result:?}"
    );
    assert!(elapsed < watchdog);
    Ok(())
}

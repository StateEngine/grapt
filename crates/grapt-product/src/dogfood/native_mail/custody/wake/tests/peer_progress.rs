//! Real retained HTTP owners must share local-only sweeps with later Sessions.
use super::*;
use crate::dogfood::native_mail::tests::http::request;
use grapt_store::mailbox::remote::progress::RecordMailPage;
use grapt_store::RemoteMailboxInput;
use grapt_types::{MailboxBody, ProtocolVersion, SeatMessageKind};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn seed_settled_quiet(application: &ServerApplication, scope: &RemoteMailboxScope) -> TestResult {
    let protocol = progress::MailTransportVersion::V2;
    let mut inbox = application.lock_inbox();
    inbox
        .store_mut()
        .prepare_remote_mail_progress(scope, protocol)?;
    let mut previous = None;
    for page in 0..64 {
        let messages = (0..64)
            .map(|offset| {
                let name = format!("quiet-{}", page * 64 + offset);
                Ok(RemoteMailboxInput {
                    scope: scope.clone(),
                    protocol: ProtocolVersion::new("remote-seat-mail-transport/v1")
                        .ok_or("protocol")?,
                    source_message: RemoteMailboxMessageId::try_new(&name)?,
                    source_binding: SourceBindingId::try_new("original-quiet-binding")?,
                    source_cursor: ExclusiveSourceCursor::try_new(format!("cursor-{name}"))?,
                    sender: SourcePrincipalId::try_new("sender")?,
                    kind: SeatMessageKind::Handoff,
                    body: MailboxBody::new("already delivered quiet history")?,
                    source_created: SourceTimestamp::try_new("2026-10-13T00:00:00.000Z")?,
                    refs: Vec::new(),
                    supersedes: None,
                    superseded_by: None,
                    archive: SourceArchiveDisposition::Active,
                    attention: MailAttention::Quiet,
                })
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        let checkpoint = messages
            .last()
            .ok_or("page checkpoint")?
            .source_cursor
            .clone();
        let acks = inbox.store_mut().record_remote_mail_page(RecordMailPage {
            scope,
            protocol,
            previous: previous.as_ref(),
            checkpoint: &checkpoint,
            messages: &messages,
            gaps: &[],
            decode_failures: &[],
            created_ms: 1,
        })?;
        for ack in acks {
            inbox
                .store_mut()
                .settle_remote_mailbox_delivery(scope, &ack, None)?;
        }
        previous = Some(checkpoint);
    }
    Ok(())
}

async fn quiet_peer(listener: tokio::net::TcpListener) -> TestResult {
    let profile: Value = serde_json::from_str(include_str!(
        "../../../../../../../../fixtures/stateengine-native/stateengine-mail-profile.json"
    ))?;
    for _ in 0..8 {
        let (mut socket, _) = listener.accept().await?;
        let incoming = request(&mut socket, 2).await?;
        let operation = incoming["operation"].as_str().ok_or("operation")?;
        let value = match operation {
            "open" => {
                json!({"binding_id":"bind-native","protocol":"remote-seat-mail-transport/v2","adapter_version":"stateengine-native-mail/2","projects":{"source":"secode","target":"secode"},"verified_principal":"quiet-owner","local_recipient":"quiet-owner","mapping_version":"exact-seat-id/v1","takeover":{"generation":1,"displaced":null},"limits":profile["limits"],"capabilities":{"atomic_subscribe_and_snapshot":true,"durable_delivery_ack":true,"bulk_dismiss":true,"explicit_project_pair":true,"opaque_authenticated_cursors":true,"advisory_payload_bytes":0,"coalesced_pending_signals":1}})
            }
            "subscribe_and_snapshot" => {
                json!({"subscription_id":"sub-quiet","generation":1,"snapshot":{"snapshot_id":"snap-quiet","barrier":"barrier-quiet","start_after":incoming["args"]["resume"]["from_checkpoint"],"retained_origin":"retained-origin/v1"}})
            }
            "fetch_snapshot_page" => {
                json!({"messages":[],"refused_rows":[],"last_cursor":null,"next_after":null})
            }
            "next_change" => {
                let mut byte = [0];
                assert_eq!(
                    socket.read(&mut byte).await?,
                    0,
                    "quiet owner poll was not joined"
                );
                continue;
            }
            "close" => json!({"joined_tasks":0,"ticks_used":1}),
            _ => return Err("already-settled quiet sweep performed remote work".into()),
        };
        let body = serde_json::to_vec(&json!({"ok":true,"value":value}))?;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
        socket.write_all(&body).await?;
        socket.shutdown().await?;
        if operation == "close" {
            return Ok(());
        }
    }
    Err("quiet HTTP request bound".into())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn later_owner_executes_while_earlier_settled_quiet_sweep_remains() -> TestResult {
    let (root, application, mut driver, initial) = fixture(None).await?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_peer_progress").ok_or("project")?,
        root.0.to_str().ok_or("directory")?,
        "later retained owner",
    )?
    .with_agent("default")?;
    let created = grapt_runtime::SessionPorts::new(application.inbox.clone(), &authorities)
        .create(&seed)?
        .session;
    let (quiet, current) = if initial < created {
        (initial, created)
    } else {
        (created, initial)
    };
    let native = application.native_mail.as_ref().ok_or("native owners")?;
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let current_port = NonZeroU16::new(listener_b.local_addr()?.port()).ok_or("port")?;
    let identity = |recipient| {
        json!({"scope":"project","project_id":"secode","agent_id":recipient,"mail_transport_protocol":"remote-seat-mail-transport/v2"}).to_string()
    };
    InstructionService::put_entry(
        application.lock_inbox().store_mut(),
        &quiet,
        &InstructionKey::new("stateengine-identity").ok_or("key")?,
        &identity("quiet-owner"),
        1,
    )?;
    bind_capability(&application, &quiet).await?;
    let owner_a = native
        .snapshot()?
        .into_iter()
        .find(|(id, _)| id == &quiet)
        .ok_or("quiet owner")?
        .1;
    let scope = {
        let mut context = owner_a.context.lock().await;
        context.use_test_port(NonZeroU16::new(listener_a.local_addr()?.port()).ok_or("port")?);
        context
            .current_binding()
            .map_err(|e| e.to_string())?
            .ok_or("quiet binding")?
            .scope
    };
    seed_settled_quiet(&application, &scope)?;
    let provider = crate::dogfood::test_provider::server(&application)?;
    let exercise = async {
        for _ in 0..1024 {
            native.drive_one(quiet.clone(), owner_a.clone()).await;
            if owner_a
                .context
                .lock()
                .await
                .runtime
                .as_ref()
                .is_some_and(HttpMailbox::is_live)
            {
                break;
            }
        }
        assert!(
            owner_a
                .context
                .lock()
                .await
                .runtime
                .as_ref()
                .is_some_and(HttpMailbox::delivery_work_pending),
            "quiet sweep was not armed at the live boundary"
        );
        InstructionService::put_entry(
            application.lock_inbox().store_mut(),
            &current,
            &InstructionKey::new("stateengine-identity").ok_or("key")?,
            &identity("coder"),
            1,
        )?;
        bind_capability(&application, &current).await?;
        let owner_b = native
            .snapshot()?
            .into_iter()
            .find(|(id, _)| id == &current)
            .ok_or("later owner")?
            .1;
        owner_b.context.lock().await.use_test_port(current_port);
        let observed = async {
            loop {
                if let Some(request) = provider.request()? {
                    let addressed = request["input"].to_string();
                    if !addressed.contains("mail-a") && !addressed.contains("mail-b") {
                        return Err("provider did not receive addressed current mail".into());
                    }
                    let fact: Value = {
                        let inbox = application.lock_inbox();
                        let text: String = inbox.store().connection().query_row(
                            "SELECT data FROM event WHERE aggregate_id=?1 AND type='session.input.admitted.1' ORDER BY seq DESC LIMIT 1",
                            [current.as_str()], |row| row.get(0))?;
                        serde_json::from_str(&text)?
                    };
                    assert_eq!(
                        fact.pointer("/input/nativeMailWake"),
                        Some(&Value::Bool(true)),
                        "later execution was not addressed native work"
                    );
                    let quiet_context = owner_a.context.lock().await;
                    let unfinished = quiet_context
                        .runtime
                        .as_ref()
                        .is_some_and(HttpMailbox::delivery_work_pending)
                        || quiet_context.advance.is_some();
                    if !unfinished {
                        return Err(
                            "later actual input waited for unrelated settled quiet sweep".into(),
                        );
                    }
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
                {
                    let held = application.lock_inbox();
                    let notices = grapt_session::notice::not_run_yet_notices(
                        held.store().connection(),
                        &current,
                    )?;
                    if let Some(notice) = notices.last() {
                        return Err(format!("provider not reached: {}", notice.reason).into());
                    }
                }
                tokio::task::yield_now().await;
            }
        };
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (run, proof) = tokio::join!(
            driver.run_until(async {
                let _stopped = stopped.await;
            }),
            async {
                let result = observed.await;
                let _joined = stop.send(());
                result
            }
        );
        proof?;
        run?;
        assert!(!owner_a.context.lock().await.owns_runtime());
        assert!(!owner_b.context.lock().await.owns_runtime());
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (a, b, proof) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        tokio::join!(
            quiet_peer(listener_a),
            fake::serve(listener_b, None, 2),
            exercise
        )
    })
    .await?;
    proof?;
    a?;
    b?;
    Ok(())
}

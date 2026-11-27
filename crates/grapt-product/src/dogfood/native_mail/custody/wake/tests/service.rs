//! Real authenticated capability ingress and the unmodified driver select.
use super::*;
use crate::dogfood::{production::tests::projection_target, DogfoodApplication};
use grapt_store::EventStore;
use grapt_types::{DeterministicAuthorities, SessionId};
#[path = "ingress.rs"]
mod ingress;
#[path = "interleaving.rs"]
mod interleaving;
#[path = "peer_progress.rs"]
mod peer_progress;
#[cfg(feature = "loopback-provider-testkit")]
#[path = "production_fixture.rs"]
mod production_fixture;
#[path = "repair.rs"]
mod repair;
use ingress::bind_capability;
#[path = "settled.rs"]
mod settled;
use settled::assert_settled_wake_coverage;
#[path = "cancellation.rs"]
mod cancellation;
#[path = "capacity.rs"]
mod capacity;
#[path = "deletion.rs"]
mod deletion;

pub(super) use crate::native_mail_test_peer as fake;
#[path = "flapping.rs"]
mod flapping;
#[path = "history.rs"]
mod history;
#[path = "observation.rs"]
mod observation;
#[path = "public_input_metadata.rs"]
mod public_input_metadata;
#[path = "rebind.rs"]
mod rebind;
#[path = "recovery.rs"]
mod recovery;
#[path = "restart.rs"]
mod restart;
#[path = "stall.rs"]
mod stall;
#[path = "stopped.rs"]
mod stopped;
#[path = "target.rs"]
mod target;

struct Root(std::path::PathBuf);
impl Root {
    fn create(path: std::path::PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

type Fixture = (
    Root,
    Arc<DogfoodApplication>,
    crate::dogfood::prompt_driver::PromptDriver,
    SessionId,
);

async fn fixture(identity: Option<&str>) -> Result<Fixture, Box<dyn std::error::Error>> {
    let (root, application, driver, session) = unbound_fixture(identity).await?;
    bind_capability(&application, &session).await?;
    Ok((root, application, driver, session))
}

async fn unbound_fixture(identity: Option<&str>) -> Result<Fixture, Box<dyn std::error::Error>> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let root = Root::create(std::env::temp_dir().join(format!(
        "grapt-seat03-driver-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )))?;
    let peer = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/stateengine-native/fixture_server.py");
    std::fs::write(
        root.0.join("grapt.jsonc"),
        serde_json::to_vec(&serde_json::json!({
            "mcp":{"stateengine":{"type":"local","codemode":false,"command":[
                if cfg!(windows) {"python"} else {"python3"}, peer,
                "--project","secode","--agent","coder","--capability","fixture-secret"
            ]}}
        }))?,
    )?;
    let hub = crate::dogfood::event_hub()?;
    let (application, driver) = crate::dogfood::test_provider::open(
        &root.0,
        EventStore::open_in_memory()?,
        &projection_target()?,
        hub.clone(),
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_native_wake").ok_or("project")?,
        root.0.to_str().ok_or("fixture directory")?,
        "idle governed seat",
    )?
    .with_agent("default")?;
    let session = grapt_runtime::SessionPorts::new(application.inbox.clone(), &authorities)
        .create(&seed)?
        .session;
    if let Some(identity) = identity {
        InstructionService::put_entry(
            application.lock_inbox().store_mut(),
            &session,
            &InstructionKey::new("stateengine-identity").ok_or("key")?,
            identity,
            1,
        )?;
    }
    Ok((root, application, driver, session))
}

#[test]
fn fixture_directory_collision_preserves_existing_bytes() -> TestResult {
    let root = Root::create(std::env::temp_dir().join(format!(
        "grapt-native-mail-collision-{}",
        std::process::id()
    )))?;
    let sentinel = root.0.join("sentinel");
    std::fs::write(&sentinel, b"existing fixture bytes")?;
    assert!(Root::create(root.0.clone()).is_err());
    assert!(
        sentinel.try_exists()?,
        "collision refusal removed existing fixture bytes"
    );
    assert_eq!(std::fs::read(&sentinel)?, b"existing fixture bytes");
    Ok(())
}

#[tokio::test]
async fn seat03_idle_driver_injects_one_burst_without_an_earlier_prompt_or_started_wait(
) -> TestResult {
    idle_burst(false).await
}

#[tokio::test]
async fn seat03_a_source_rescan_wakes_even_when_the_attention_count_is_unchanged() -> TestResult {
    idle_burst(true).await
}

async fn idle_burst(unchanged_count: bool) -> TestResult {
    let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owners = native.snapshot()?;
    assert_eq!(owners.len(), 1, "idle startup required an earlier prompt");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    {
        let mut context = owners[0].1.context.lock().await;
        context.use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
        if unchanged_count {
            // A source rescan can replace delivered rows without changing the
            // published count. Only the source signal requests admission.
            context.attention = MailAttentionInfo::Ready {
                pending: MailPendingCount::new(2).ok_or("count")?,
                incomplete: false,
                decode_failures: MailPendingCount::new(0).ok_or("count")?,
            };
            context.verified_revision = Some(owners[0].1.revision()?);
            context.wake_scan_requested = true;
        }
    }
    // Stop at the durable injection boundary, before polling the queued job.
    // Awaiting Started inside the mail branch would deadlock this witness.
    let stop = std::future::poll_fn(|_| {
        let pending = application.lock_inbox().pending_native_mail_wake(&session);
        match pending {
            Ok(Some(_)) => Poll::Ready(()),
            Ok(None) => Poll::Pending,
            Err(error) => panic!("native injection evidence unavailable: {error}"),
        }
    });
    let (transport, run) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(fake::serve(listener, None, 2), driver.run_until(stop))
    })
    .await?;
    let calls = transport?;
    run?;
    assert_eq!(
        calls.iter().filter(|call| call.as_str() == "open").count(),
        1
    );
    assert_eq!(calls.last().map(String::as_str), Some("close"));
    let inbox = application.lock_inbox();
    let pending = inbox.pending_inputs(&session)?;
    assert_eq!(pending.len(), 1, "burst injected duplicate inputs");
    assert_settled_wake_coverage(&inbox, &pending[0])?;
    assert!(!pending[0].text.contains("quiet"));
    assert!(!pending[0].text.contains("fixture-secret"));
    assert!(
        inbox.pending_native_mail_wake(&session)?.is_some(),
        "shutdown lost the admitted input"
    );
    Ok(())
}

#[tokio::test]
async fn seat03_ordinary_attended_and_incognito_sessions_do_not_acquire_wake_owners() -> TestResult
{
    for identity in [None, Some(r#"{"scope":"incognito"}"#)] {
        let (_root, application, mut driver, session) = fixture(identity).await?;
        let native = application.native_mail.as_ref().ok_or("native owners")?;
        assert!(
            native.snapshot()?.is_empty(),
            "attended session acquired a native consumer"
        );
        driver.run_until(std::future::ready(())).await?;
        assert!(application
            .lock_inbox()
            .pending_inputs(&session)?
            .is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn seat03_capacity_release_retries_an_idle_wake_without_another_mail_signal() -> TestResult {
    let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owners = native.snapshot()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owners[0]
        .1
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let mut held = Vec::new();
    for _ in 0..crate::dogfood::MAX_ACTIVE_EXECUTIONS {
        held.push(application.prompt_mailbox.reserve()?);
    }
    let (entered, poll) = tokio::sync::oneshot::channel();
    let stop = std::future::poll_fn(|_| {
        match application.lock_inbox().pending_native_mail_wake(&session) {
            Ok(Some(_)) => Poll::Ready(()),
            Ok(None) => Poll::Pending,
            Err(error) => panic!("capacity witness unreadable: {error}"),
        }
    });
    let witness = async {
        poll.await?;
        for _ in 0..64 {
            if native.capacity_waiting() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            native.capacity_waiting(),
            "full capacity did not retain an admission wait"
        );
        assert!(
            application
                .lock_inbox()
                .pending_inputs(&session)?
                .is_empty(),
            "mail committed before execution capacity was reserved"
        );
        let scope = owners[0]
            .1
            .context
            .lock()
            .await
            .binding
            .as_ref()
            .ok_or("binding")?
            .scope
            .clone();
        let page = application
            .lock_inbox()
            .store()
            .read_remote_mailbox_for_binding(
                &scope,
                grapt_types::MailboxCursor::START,
                64,
                4194304,
            )?;
        assert_eq!(
            page.records.iter().filter(|row| row.actionable()).count(),
            2,
            "capacity refusal consumed pending mail"
        );
        drop(held.pop().ok_or("reserved capacity")?);
        // No mail, timer, attention-count change, or manual wake follows.
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (transport, run, witnessed) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(
                fake::serve(listener, Some(entered), 2),
                driver.run_until(stop),
                witness
            )
        })
        .await?;
    transport?;
    run?;
    witnessed?;
    assert_eq!(application.lock_inbox().pending_inputs(&session)?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn seat03_legacy_v1_attention_does_not_enable_automatic_wake() -> TestResult {
    let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v1"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owners = native.snapshot()?;
    let owner = owners.first().ok_or("legacy attention owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let mut wake_boundary_requested = false;
    let stop = std::future::poll_fn(|_| {
        if wake_boundary_requested {
            return Poll::Ready(());
        }
        if owner.context.try_lock().is_ok_and(|context| {
            context
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.is_live())
        }) {
            // Let the real select handle a change and reach its next wake
            // admission boundary before the biased stop can finish the test.
            wake_boundary_requested = true;
            native.changed.notify_one();
        }
        Poll::Pending
    });
    let (transport, run) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(fake::serve(listener, None, 1), driver.run_until(stop))
    })
    .await?;
    transport?;
    run?;
    assert!(
        application
            .lock_inbox()
            .pending_inputs(&session)?
            .is_empty(),
        "legacy attention was upgraded into autonomous execution"
    );
    Ok(())
}

#[tokio::test]
async fn seat03_the_driver_rechecks_revocation_after_a_busy_wait_before_provider_effects(
) -> TestResult {
    let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owners = native.snapshot()?;
    let owner = owners.first().ok_or("native owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let (started, _accepted) = crate::dogfood::prompt_driver::Started::channel();
    let (_cancel, cancelled) = tokio::sync::watch::channel(false);
    let active = application
        .wake_slots
        .reserve(&session)?
        .enter(&started, cancelled)
        .await?
        .ok_or("active Session custody")?;
    let queued = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (entered, poll) = tokio::sync::oneshot::channel();
    let stop = std::future::poll_fn(|_| {
        if queued.load(Ordering::SeqCst) && !owner.wake_scheduled.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let witness = async {
        poll.await?;
        for _ in 0..1024 {
            if owner.wake_scheduled.load(Ordering::SeqCst)
                && application
                    .lock_inbox()
                    .pending_native_mail_wake(&session)?
                    .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            owner.wake_scheduled.load(Ordering::SeqCst),
            "native input never acquired execution custody"
        );
        assert!(
            application
                .lock_inbox()
                .pending_native_mail_wake(&session)?
                .is_some(),
            "native admission missing before revocation"
        );
        assert!(
            crate::dogfood::test_provider::server(&application)?
                .request()?
                .is_none(),
            "busy native input reached the provider early"
        );
        queued.store(true, Ordering::SeqCst);
        // Real authenticated rebind, even though the secret's bytes are equal.
        bind_capability(&application, &session).await?;
        drop(active);
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (transport, run, witnessed) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(
                fake::serve(listener, Some(entered), 2),
                driver.run_until(stop),
                witness
            )
        })
        .await?;
    transport?;
    run?;
    witnessed?;
    assert!(
        crate::dogfood::test_provider::server(&application)?
            .request()?
            .is_none(),
        "revoked queued wake reached provider effects"
    );
    assert!(
        !application
            .lock_inbox()
            .pending_inputs(&session)?
            .is_empty(),
        "revocation lost the admitted input"
    );
    Ok(())
}

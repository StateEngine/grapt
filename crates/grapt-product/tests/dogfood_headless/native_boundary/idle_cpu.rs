//! An idle service's main thread parks between events; it never spins a core.
//!
//! The witness reads the service's main task from /proc: CPU ticks over a quiet
//! window must stay under 5% of one core. A task that stays parked for the whole
//! window makes no new context switches, so switches alone prove nothing; the
//! CPU bound is the invariant.
#![cfg(target_os = "linux")]
use super::*;

/// One core's quiet window, and the share of it an idle service may use.
const WINDOW: Duration = Duration::from_secs(5);
/// Linux reports task times in USER_HZ, which is 100 on every supported target.
const TICKS_PER_SECOND: u64 = 100;
const IDLE_PERCENT: u64 = 5;

/// User plus system ticks of the process main task.
fn main_task(pid: u32) -> u64 {
    let task = format!("/proc/{pid}/task/{pid}");
    let stat = fs::read_to_string(format!("{task}/stat"))
        .unwrap_or_else(|error| panic!("main task stat unreadable: {error}"));
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace().collect())
        .unwrap_or_default();
    let field = |index: usize| -> u64 {
        fields
            .get(index)
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("main task stat field {index} missing: {stat}"))
    };
    // After the command name: state is 0, so utime (14) is 11 and stime (15) 12.
    field(11) + field(12)
}

fn assert_main_thread_parks(child: &process_fixture::ManagedChild, state: &str) {
    let pid = child.id();
    let before = main_task(pid);
    thread::sleep(WINDOW);
    let used = main_task(pid).saturating_sub(before);
    let window = WINDOW.as_secs() * TICKS_PER_SECOND;
    assert!(
        used < window * IDLE_PERCENT / 100,
        "{state}: the idle main thread used {used} of {window} ticks in {WINDOW:?}"
    );
}

/// Attach one window to `session` on /api/event, then lose it silently, so the
/// Session enters its close grace with no window watching.
fn lose_a_window(address: SocketAddr, session: &str) {
    let mut stream =
        TcpStream::connect(address).unwrap_or_else(|error| panic!("window connect: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap_or_else(|error| panic!("window timeout: {error}"));
    write!(
        stream,
        "GET /api/event?session={session} HTTP/1.1\r\nHost: {address}\r\nAuthorization: {AUTHORIZATION}\r\nConnection: close\r\n\r\n"
    )
    .unwrap_or_else(|error| panic!("window request: {error}"));
    let mut head = [0_u8; 12];
    stream
        .read_exact(&mut head)
        .unwrap_or_else(|error| panic!("window handshake: {error}"));
    assert_eq!(&head, b"HTTP/1.1 200", "window refused");
    drop(stream);
}

#[test]
fn an_idle_service_main_thread_parks() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("idle-cpu-plain");
    let _root_cleanup = super::super::terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 90_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut child = process_fixture::spawn_controlled(&root, provider.address);
    let _address = await_ready(&root, &mut child);
    assert_main_thread_parks(&child, "no sessions");
    stop_cleanly(child, &root)
}

#[test]
fn an_idle_service_with_bound_mail_parks_before_and_after_restart() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("idle-cpu-mail");
    let _root_cleanup = super::super::terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 90_000);
    stateengine_native::configure(&root, "ready");
    super::super::close_grace::lease::set_close_grace(&root, 4_000);
    let mail = mail::Mail::start()?;
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["permission"] = serde_json::json!({"*":"allow"});
    config["stateengine"] =
        serde_json::json!({"native_mail_endpoint":format!("http://{}",mail.address)});
    fs::write(&path, serde_json::to_vec(&config)?)?;
    let provider = ProviderFixture::start(vec![Script::SuccessText("IDLE_MAIL_ANSWER")]);
    let mut first = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut first);
    let session = create_restart_session(address, &root)?;
    mail.arrive(1);
    mail.wait_delivered(1);
    wait_calls(&provider.calls, 1);
    permission_residuals::wait_session_idle(address, &session);
    assert_main_thread_parks(&first, "bound mail, delivered and idle");
    // Re-provisioning the live binding retires the earlier owner.
    stateengine_binding::put_no_content(
        address,
        &format!("/api/session/{session}/stateengine/capability"),
        r#"{"capability":"NATIVE_FIXTURE_CAPABILITY"}"#,
    );
    assert_main_thread_parks(&first, "bound mail after re-provision");
    // The only window goes away without a word: the close grace runs, then ends.
    lose_a_window(address, &session);
    assert_main_thread_parks(&first, "bound mail, window lost, grace running");
    assert_main_thread_parks(&first, "bound mail, grace expired");
    // A second Session bound to the same native identity takes over its mailbox.
    let successor = create_restart_session(address, &root)?;
    assert_main_thread_parks(&first, "a second Session bound to the same identity");
    stop_cleanly(first, &root)?;

    // A restart leaves the bound mailbox waiting for a fresh capability.
    let mut second = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut second);
    require_rebind_attention(address, &successor);
    assert_main_thread_parks(&second, "bound mail awaiting rebind");
    // Mail that arrives while the capability is missing is retained as debt.
    mail.arrive(2);
    require_rebind_attention(address, &successor);
    assert_main_thread_parks(&second, "rebind required with retained mail");
    stop_cleanly(second, &root)
}

/// Admit and settle `count` mails for the fixture identity. Finished wake
/// mail is consumed; quiet mail is never actionable, so it is never consumed
/// and stays open, as most of a real seat's mailbox does.
pub(super) fn seed_history(root: &Path, count: usize, attention: MailAttentionKind) -> TestResult {
    use grapt_store::{RemoteMailboxInput, RemoteMailboxScope};
    use grapt_types::remote_mail::*;
    use grapt_types::{MailboxBody, ProtocolVersion, SeatMessageKind, SeatRecipientId};
    let scope = RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new("native-fixture")?,
            TargetProjectId::try_new("native-fixture")?,
        ),
        principal: SourcePrincipalId::try_new("agt_native_fixture")?,
        recipient: SeatRecipientId::try_new("agt_native_fixture")?,
    };
    let mut store = EventStore::open(&root.join("database.sqlite3"), "idle-cpu-seed")?;
    let tx = store.governed_transaction()?;
    for index in 0..count {
        let input = RemoteMailboxInput {
            scope: scope.clone(),
            protocol: ProtocolVersion::new("remote-seat-mail-transport/v1").ok_or("protocol")?,
            source_message: RemoteMailboxMessageId::try_new(format!("history-{index}"))?,
            source_binding: SourceBindingId::try_new("bind-native")?,
            source_cursor: ExclusiveSourceCursor::try_new(format!("cursor-history-{index}"))?,
            sender: SourcePrincipalId::try_new("pilot")?,
            kind: SeatMessageKind::Handoff,
            body: MailboxBody::new(format!("finished history mail {index}"))?,
            source_created: SourceTimestamp::try_new("2026-10-13T00:00:00.000Z")?,
            refs: Vec::new(),
            supersedes: None,
            superseded_by: None,
            archive: SourceArchiveDisposition::Active,
            attention: match attention {
                MailAttentionKind::Consumed => MailAttention::Wake,
                MailAttentionKind::Quiet => MailAttention::Quiet,
            },
        };
        let ack = EventStore::admit_remote_mailbox_within(&tx, &scope, &input)?.ack;
        EventStore::settle_remote_mailbox_delivery_within(
            &tx,
            &scope,
            &ack,
            Some(SecondaryProjectionState::Posted),
        )?;
        if matches!(attention, MailAttentionKind::Consumed) {
            EventStore::consume_remote_mailbox_within(&tx, &scope, ack.local_message())?;
        }
    }
    tx.commit()?.discard();
    Ok(())
}

/// What the seeded history is: consumed wake mail, or open quiet mail.
#[derive(Clone, Copy)]
pub(super) enum MailAttentionKind {
    Consumed,
    Quiet,
}

#[test]
fn a_rebound_mailbox_with_long_consumed_history_parks() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("idle-cpu-history");
    let _root_cleanup = super::super::terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 90_000);
    stateengine_native::configure(&root, "ready");
    let mail = mail::Mail::start()?;
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["permission"] = serde_json::json!({"*":"allow"});
    config["stateengine"] =
        serde_json::json!({"native_mail_endpoint":format!("http://{}",mail.address)});
    fs::write(&path, serde_json::to_vec(&config)?)?;
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("HISTORY_FIRST"),
        Script::SuccessText("HISTORY_SECOND"),
    ]);
    let mut first = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut first);
    let session = create_restart_session(address, &root)?;
    mail.arrive(1);
    mail.wait_delivered(1);
    wait_calls(&provider.calls, 1);
    permission_residuals::wait_session_idle(address, &session);
    stop_cleanly(first, &root)?;
    seed_history(&root, 10_000, MailAttentionKind::Consumed)?;

    // Re-provisioning starts the admission scan over the whole mailbox.
    let mut second = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut second);
    require_rebind_attention(address, &session);
    stateengine_binding::put_no_content(
        address,
        &format!("/api/session/{session}/stateengine/capability"),
        r#"{"capability":"NATIVE_FIXTURE_CAPABILITY"}"#,
    );
    // A rebind pays one bounded walk over the history, yielding as it goes:
    // mail that arrives during it is still delivered and wakes the session.
    mail.arrive(2);
    mail.wait_delivered(2);
    wait_calls(&provider.calls, 2);
    // The walk ends within a bound proportional to the rows, then parks.
    let walk = PollDeadline::within(Duration::from_secs(30));
    while main_task_share(&second, Duration::from_secs(1)) >= IDLE_PERCENT {
        assert!(
            walk.pending(),
            "the rebind walk over 10,000 rows did not end"
        );
    }
    assert_main_thread_parks(&second, "after the rebind walk");
    // A later source observation reads only rows above the high-water mark.
    mail.quiet_boundary();
    assert_main_thread_parks(&second, "after a later observation");
    stop_cleanly(second, &root)
}

/// The main task's share of one core, in percent, over `window`.
fn main_task_share(child: &process_fixture::ManagedChild, window: Duration) -> u64 {
    let before = main_task(child.id());
    thread::sleep(window);
    let used = main_task(child.id()).saturating_sub(before);
    let millis = u64::try_from(window.as_millis()).unwrap_or(u64::MAX).max(1);
    used * 1000 / TICKS_PER_SECOND * 100 / millis
}

#[test]
fn advisories_over_a_long_open_quiet_history_cost_the_main_thread_nothing() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("idle-cpu-quiet");
    let _root_cleanup = super::super::terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 90_000);
    stateengine_native::configure(&root, "ready");
    let mail = mail::Mail::start()?;
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    config["permission"] = serde_json::json!({"*":"allow"});
    config["stateengine"] =
        serde_json::json!({"native_mail_endpoint":format!("http://{}",mail.address)});
    fs::write(&path, serde_json::to_vec(&config)?)?;
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("QUIET_FIRST"),
        Script::SuccessText("QUIET_SECOND"),
    ]);
    let mut first = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut first);
    let session = create_restart_session(address, &root)?;
    mail.arrive(1);
    mail.wait_delivered(1);
    wait_calls(&provider.calls, 1);
    permission_residuals::wait_session_idle(address, &session);
    stop_cleanly(first, &root)?;
    seed_history(&root, 10_000, MailAttentionKind::Quiet)?;

    let mut second = process_fixture::spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut second);
    require_rebind_attention(address, &session);
    stateengine_binding::put_no_content(
        address,
        &format!("/api/session/{session}/stateengine/capability"),
        r#"{"capability":"NATIVE_FIXTURE_CAPABILITY"}"#,
    );
    mail.arrive(2);
    mail.wait_delivered(2);
    wait_calls(&provider.calls, 2);
    // The first pass after a bind reads the open history once, then parks.
    let walk = PollDeadline::within(Duration::from_secs(30));
    while main_task_share(&second, Duration::from_secs(1)) >= IDLE_PERCENT {
        assert!(
            walk.pending(),
            "the first pass over 10,000 open rows did not end"
        );
    }
    // Each advisory brings no new mail: it must not re-read the open history.
    let before = main_task(second.id());
    for phase in 3..=10 {
        mail.advise(phase);
    }
    thread::sleep(Duration::from_secs(2));
    let used = main_task(second.id()).saturating_sub(before);
    eprintln!("advisory main-thread ticks: {used}");
    assert!(
        used < TICKS_PER_SECOND / 2,
        "8 advisories over 10,000 open quiet mails used {used} main-thread ticks"
    );
    assert_main_thread_parks(&second, "after the advisories");
    stop_cleanly(second, &root)
}

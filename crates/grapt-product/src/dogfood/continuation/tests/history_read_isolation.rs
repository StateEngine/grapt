//! Authenticated read routes must finish while the writer owns its mutex.
use super::*;
use base64::Engine;
use std::sync::{atomic::AtomicBool, mpsc};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn service(
    app: &Arc<crate::dogfood::DogfoodApplication>,
) -> Result<grapt_api::AuthenticatedApiService, String> {
    let config = crate::input::ServiceConfig {
        slot: "read-isolation".into(),
        instance: "read-isolation".into(),
        environment: "fixture".into(),
        build: "fixture".into(),
        version: "fixture".into(),
        serve_window_ms: 10_000,
        provider_connect_ms: 1000,
        provider_response_head_ms: 1000,
        provider_active_attempts: 1,
        close_grace_ms: None,
        max_inflight_streams: None,
    };
    let credential = crate::input::Credential {
        username: "reader".into(),
        secret: "synthetic-reader-secret".into(),
    };
    let ports =
        crate::compose::DogfoodComposition::ready(app.clone(), app.clone(), app.hub.clone(), None);
    crate::compose_host::product_service(
        &crate::compose_host::host_limits().map_err(|error| error.to_string())?,
        &credential,
        grapt_types::service::ProcessId::new(std::process::id()).ok_or("process id")?,
        &config,
        Some(ports),
    )
    .map_err(|error| error.to_string())
}

async fn get(address: std::net::SocketAddr, path: &str) -> Result<String, String> {
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .map_err(|error| error.to_string())?;
    let credential =
        base64::engine::general_purpose::STANDARD.encode("reader:synthetic-reader-secret");
    stream.write_all(format!(
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Basic {credential}\r\nConnection: close\r\n\r\n"
    ).as_bytes()).await.map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    stream
        .take(1024 * 1024)
        .read_to_end(&mut response)
        .await
        .map_err(|error| error.to_string())?;
    if response.len() == 1024 * 1024 {
        return Err("HTTP response exceeded the fixture read bound".into());
    }
    String::from_utf8(response).map_err(|error| error.to_string())
}

async fn independent_read(route: &str) -> Result<(), String> {
    let (fixture, app, _driver) = long_session("http-reader-isolation")?;
    let path = match route {
        "list" => "/api/session?limit=1".to_owned(),
        "info" => format!("/api/session/{}", fixture.session),
        "history" => format!("/api/session/{}/message?limit=1", fixture.session),
        "message" => {
            let message = app
                .message_history(&fixture.session)?
                .pop()
                .ok_or("assistant message")?;
            format!(
                "/api/session/{}/message/{}",
                fixture.session, message.message
            )
        }
        _ => return Err("unknown read route".into()),
    };
    let listener = crate::compose_host::bind_loopback()
        .await
        .map_err(|error| error.to_string())?;
    let host = grapt_api::PreparedApiHost::new(
        listener,
        service(&app)?,
        crate::compose_host::host_limits().map_err(|error| error.to_string())?,
        Arc::new(grapt_api::TokioDeadlineDriver),
    )
    .map_err(|error| error.to_string())?
    .start();
    let address = host.ready().observed_address();
    let inbox = app.inbox.clone();
    let ticked = Arc::new(AtomicBool::new(false));
    let observed = ticked.clone();
    let (held, ready) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let writer = std::thread::spawn(move || -> Result<(bool, bool), String> {
        let guard = inbox.lock().map_err(|_| "writer mutex poisoned")?;
        guard
            .store()
            .connection()
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|error| error.to_string())?;
        held.send(()).map_err(|error| error.to_string())?;
        // An external deadline releases a defective async route without
        // depending on the runtime whose responsiveness this test judges.
        let finished_while_held = released.recv_timeout(Duration::from_secs(2)).is_ok();
        let timer_fired_while_held = observed.load(Ordering::Acquire);
        guard
            .store()
            .connection()
            .execute_batch("ROLLBACK")
            .map_err(|error| error.to_string())?;
        Ok((finished_while_held, timer_fired_while_held))
    });
    ready
        .recv_timeout(Duration::from_secs(3))
        .map_err(|error| error.to_string())?;
    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        ticked.store(true, Ordering::Release);
    });
    let response = tokio::time::timeout(Duration::from_millis(750), get(address, &path)).await;
    timer.await.map_err(|error| error.to_string())?;
    let _sent = release.send(());
    let result = writer.join().map_err(|_| "writer thread panicked")?;
    host.stop().await;
    let (finished, responsive) = result?;
    assert!(
        responsive,
        "{route} blocked the async timer while waiting for the writer"
    );
    assert!(finished, "{route} required the writer mutex to be released");
    let response = response.map_err(|_| format!("{route} exceeded the read deadline"))??;
    assert!(
        response.starts_with("HTTP/1.1 200 "),
        "{route} refused: {response}"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn session_list_http_does_not_wait_for_writer_ownership() -> Result<(), String> {
    independent_read("list").await
}

#[tokio::test(flavor = "current_thread")]
async fn session_info_http_does_not_wait_for_writer_ownership() -> Result<(), String> {
    independent_read("info").await
}

#[tokio::test(flavor = "current_thread")]
async fn history_page_http_does_not_wait_for_writer_ownership() -> Result<(), String> {
    independent_read("history").await
}

#[tokio::test(flavor = "current_thread")]
async fn single_message_http_does_not_wait_for_writer_ownership() -> Result<(), String> {
    independent_read("message").await
}

#[tokio::test(flavor = "current_thread")]
async fn saturated_history_http_returns_unavailable_while_writer_and_timers_progress(
) -> Result<(), String> {
    let (fixture, app, _driver) = long_session("http-reader-capacity")?;
    let pool = lock(&app.inbox)
        .store()
        .read_pool()
        .map_err(|error| error.to_string())?
        .ok_or("file reader pool absent")?;
    let mut held = Vec::new();
    for _ in 0..grapt_store::read_pool::READ_CAPACITY {
        held.push(pool.checkout().map_err(|error| error.to_string())?);
    }
    let host = grapt_api::PreparedApiHost::new(
        crate::compose_host::bind_loopback()
            .await
            .map_err(|error| error.to_string())?,
        service(&app)?,
        crate::compose_host::host_limits().map_err(|error| error.to_string())?,
        Arc::new(grapt_api::TokioDeadlineDriver),
    )
    .map_err(|error| error.to_string())?
    .start();
    let address = host.ready().observed_address();
    let path = format!("/api/session/{}/message?limit=1", fixture.session);
    let request = tokio::spawn(async move { get(address, &path).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let input: MessageId = "msg_during_saturated_read"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(&app.inbox, &fixture.session, &input, "writer progressed")?;
    let response = tokio::time::timeout(Duration::from_secs(1), request).await;
    drop(held);
    host.stop().await;
    let response = response
        .map_err(|_| "saturated read never reached its deadline")?
        .map_err(|error| error.to_string())??;
    assert!(
        response.starts_with("HTTP/1.1 503 "),
        "capacity refusal was not retryable: {response}"
    );
    assert!(
        response.contains("ServiceUnavailableError"),
        "capacity refusal lost its typed body: {response}"
    );
    assert!(
        response.to_ascii_lowercase().contains("retry-after: 1\r\n"),
        "capacity refusal lost its retry guidance: {response}"
    );
    Ok(())
}

#[test]
fn provider_confirmed_usage_does_not_wait_for_writer_ownership() -> Result<(), String> {
    let fixture = Fixture::new("usage-read-isolation")?;
    fixture.record_exchange()?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let attempt = PhysicalAttemptId::new("pat_d2a_read").ok_or("attempt")?;
    let guard = lock(fixture.inbox()?);
    guard
        .store()
        .connection()
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| error.to_string())?;
    let (done, received) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let _sent = done.send(continuation.confirmed_usage(&attempt));
    });
    let result = received.recv_timeout(Duration::from_secs(2));
    guard
        .store()
        .connection()
        .execute_batch("ROLLBACK")
        .map_err(|error| error.to_string())?;
    drop(guard);
    reader.join().map_err(|_| "usage reader panicked")?;
    result
        .map_err(|_| "provider usage read waited for the writer mutex")?
        .map_err(|error| format!("provider usage refused: {error:?}"))?;
    Ok(())
}

#[test]
fn history_unavailable_deadline_includes_the_blocking_dispatch_queue() -> Result<(), String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .map_err(|error| error.to_string())?
        .block_on(async {
            let (fixture, app, _driver) = long_session("http-reader-dispatch-deadline")?;
            let pool = lock(&app.inbox)
                .store()
                .read_pool()
                .map_err(|error| error.to_string())?
                .ok_or("file reader pool absent")?;
            let mut held = Vec::new();
            for _ in 0..grapt_store::read_pool::READ_CAPACITY {
                held.push(pool.checkout().map_err(|error| error.to_string())?);
            }
            let host = grapt_api::PreparedApiHost::new(
                crate::compose_host::bind_loopback()
                    .await
                    .map_err(|error| error.to_string())?,
                service(&app)?,
                crate::compose_host::host_limits().map_err(|error| error.to_string())?,
                Arc::new(grapt_api::TokioDeadlineDriver),
            )
            .map_err(|error| error.to_string())?
            .start();
            let address = host.ready().observed_address();
            let (started, ready) = mpsc::sync_channel(1);
            let (release, released) = mpsc::sync_channel(1);
            let blocking_job = tokio::task::spawn_blocking(move || {
                let _sent = started.send(());
                // External release also bounds cleanup if an assertion fails.
                let _released = released.recv_timeout(Duration::from_secs(3));
            });
            ready
                .recv_timeout(Duration::from_secs(1))
                .map_err(|error| error.to_string())?;
            let path = format!("/api/session/{}/message?limit=1", fixture.session);
            let response =
                tokio::time::timeout(Duration::from_millis(750), get(address, &path)).await;
            let _sent = release.send(());
            blocking_job.await.map_err(|error| error.to_string())?;
            drop(held);
            host.stop().await;
            let response = response
                .map_err(|_| "reader deadline started only after the blocking dispatch queue")??;
            assert!(
                response.starts_with("HTTP/1.1 503 "),
                "capacity refusal was not retryable: {response}"
            );
            Ok(())
        })
}

#[tokio::test(flavor = "current_thread")]
async fn abandoning_an_active_sql_read_releases_capacity_without_poisoning_reuse(
) -> Result<(), String> {
    cancel_active_read_after(Duration::from_millis(30)).await
}

/// The async read path is the one the HTTP routes use, so its failure path
/// needs its own witness: a projection that returns an error must close its
/// reader instead of idling the connection for the next checkout.
#[tokio::test(flavor = "current_thread")]
async fn a_failed_async_projection_does_not_recycle_its_reader_into_the_pool() -> Result<(), String>
{
    let (_fixture, app, _driver) = long_session("async-failed-projection-recycle")?;
    let pool = lock(&app.inbox)
        .store()
        .read_pool()
        .map_err(|error| error.to_string())?
        .ok_or("file reader pool absent")?;
    // Mark every connection the pool will hand out, holding them so the sweep
    // ends. Then release them and fail one projection through the async path
    // the HTTP routes use. The failed read must close the connection it held,
    // so exactly one fewer marked connection is left: other idle connections
    // stay marked, which is why this counts rather than forbidding a survivor.
    let mut sweep = Vec::new();
    while let Ok(read) = pool.checkout() {
        read.store()
            .connection()
            .pragma_update(None, "cache_size", -4321)
            .map_err(|error| error.to_string())?;
        sweep.push(read);
    }
    let marked = sweep.len();
    assert!(marked > 0, "the pool handed out no idle connection to mark");
    for read in sweep {
        read.finish().map_err(|error| error.to_string())?;
    }
    let refused = app
        .read_projection(
            |_| Err::<(), String>("database disk image is malformed".to_owned()),
            |error| error,
        )
        .await;
    assert!(
        refused.is_err(),
        "the failing projection was expected to refuse"
    );
    let mut survivors = Vec::new();
    let mut inherited = 0;
    while let Ok(read) = pool.checkout() {
        let value: i64 = read
            .store()
            .connection()
            .pragma_query_value(None, "cache_size", |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if value == -4321 {
            inherited += 1;
        }
        survivors.push(read);
    }
    for read in survivors {
        read.finish().map_err(|error| error.to_string())?;
    }
    assert_eq!(
        inherited,
        marked - 1,
        "a reader whose async projection failed was recycled into the pool"
    );
    Ok(())
}

async fn cancel_active_read_after(delay: Duration) -> Result<(), String> {
    let (_fixture, app, _driver) = long_session("http-reader-cancellation")?;
    let pool = lock(&app.inbox)
        .store()
        .read_pool()
        .map_err(|error| error.to_string())?
        .ok_or("file reader pool absent")?;
    let mut held = Vec::new();
    for _ in 0..grapt_store::read_pool::READ_CAPACITY - 1 {
        held.push(pool.checkout().map_err(|error| error.to_string())?);
    }
    let (started, ready) = tokio::sync::oneshot::channel();
    let projection = app.read_projection(
        move |reader| {
            let inbox = lock(&reader.inbox);
            let _sent = started.send(());
            // Bounded SQL work long enough to need interruption, without a new
            // test hook or a callback that changes the production query owner.
            inbox
                .store()
                .connection()
                .query_row(
                    "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n
             WHERE x<100000000) SELECT count(*) FROM n",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())
        },
        |error| error,
    );
    let task = tokio::spawn(projection);
    tokio::time::timeout(Duration::from_secs(1), ready)
        .await
        .map_err(|_| "read projection did not start")?
        .map_err(|error| error.to_string())?;
    tokio::time::sleep(delay).await;
    task.abort();
    let cancelled = task
        .await
        .err()
        .ok_or("projection completed before cancellation")?;
    assert!(
        cancelled.is_cancelled(),
        "projection failed independently of caller cancellation"
    );
    let reused = tokio::task::spawn_blocking(move || -> Result<i64, String> {
        let read = pool.checkout().map_err(|error| error.to_string())?;
        let value = read
            .store()
            .connection()
            .query_row("SELECT 7", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        read.finish().map_err(|error| error.to_string())?;
        Ok(value)
    });
    let result = tokio::time::timeout(Duration::from_secs(1), reused).await;
    drop(held);
    assert_eq!(
        result
            .map_err(|_| "cancelled read retained capacity")?
            .map_err(|error| error.to_string())??,
        7,
        "a cancelled owner's interrupt reached a later reader"
    );
    Ok(())
}

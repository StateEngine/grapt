//! A door that stops answering without closing anything: the held long poll
//! and every new request hang. Mail must still arrive once the door is back,
//! and the status must say mail is retrying meanwhile, never ready.
use super::*;
use std::sync::atomic::AtomicBool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Forwards to the door until told to hang; then it accepts and holds every
/// connection without a byte, and never closes the ones it already holds.
struct Relay {
    upstream: Arc<Mutex<std::net::SocketAddr>>,
    hung: Arc<AtomicBool>,
}

impl Relay {
    fn serve(listener: TcpListener, upstream: std::net::SocketAddr) -> Self {
        let relay = Self {
            upstream: Arc::new(Mutex::new(upstream)),
            hung: Arc::new(AtomicBool::new(false)),
        };
        let (upstream, hung) = (relay.upstream.clone(), relay.hung.clone());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((client, _)) = listener.accept().await {
                if hung.load(Ordering::SeqCst) {
                    held.push(client);
                    continue;
                }
                let target = *upstream.lock().unwrap_or_else(|e| e.into_inner());
                let Ok(server) = TcpStream::connect(target).await else {
                    held.push(client);
                    continue;
                };
                tokio::spawn(pump(client, server, hung.clone()));
            }
        });
        relay
    }
}

/// Copies both ways; once hung, nothing more is forwarded or closed.
async fn pump(client: TcpStream, server: TcpStream, hung: Arc<AtomicBool>) {
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = server.into_split();
    let up = async {
        let mut chunk = [0_u8; 16 * 1024];
        while let Ok(count) = client_read.read(&mut chunk).await {
            if count == 0 || hung.load(Ordering::SeqCst) {
                break;
            }
            if server_write.write_all(&chunk[..count]).await.is_err() {
                break;
            }
        }
    };
    let down = async {
        let mut chunk = [0_u8; 16 * 1024];
        while let Ok(count) = server_read.read(&mut chunk).await {
            if count == 0 || hung.load(Ordering::SeqCst) {
                break;
            }
            if client_write.write_all(&chunk[..count]).await.is_err() {
                break;
            }
        }
        if !hung.load(Ordering::SeqCst) {
            let _ = client_write.shutdown().await;
        }
        client_write
    };
    let (_, held) = tokio::join!(up, down);
    // A hung door keeps the client's side open until the test ends.
    while hung.load(Ordering::SeqCst) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    drop(held);
}

fn mail_status(native: &NativeMailOwners) -> Option<String> {
    native.hydrate().iter().find_map(|frame| {
        let frame = serde_json::to_value(frame).ok()?;
        frame["data"]["status"]["type"].as_str().map(str::to_owned)
    })
}

#[tokio::test]
async fn native_mail_stall_retries_visibly_and_delivers_once_after_the_door_returns() -> TestResult
{
    let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let door = TcpListener::bind("127.0.0.1:0").await?;
    let front = TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(front.local_addr()?.port()).ok_or("port")?);
    let relay = Relay::serve(front, door.local_addr()?);
    let (entered, poll) = tokio::sync::oneshot::channel();
    let mut retrying = false;
    let stop = async {
        std::future::poll_fn(|_| {
            match application.lock_inbox().pending_native_mail_wake(&session) {
                Ok(Some(_)) => Poll::Ready(()),
                Ok(None) => Poll::Pending,
                Err(error) => panic!("stall admission evidence unavailable: {error}"),
            }
        })
        .await;
    };
    let hang_then_return = async {
        let mut serving = Box::pin(fake::serve_quiet(door, entered));
        tokio::select! {
            result = &mut serving => return Err(format!("door exited before its idle poll: {result:?}")),
            result = poll => result.map_err(|e| e.to_string())?,
        }
        // The door stops answering; nothing is closed.
        relay.hung.store(true, Ordering::SeqCst);
        drop(serving);
        // Worst case to notice: the poll budget (10 s) plus the first probe's
        // bound (5 s), then a retry; allow for the scheduler.
        for _ in 0..300 {
            if mail_status(native).as_deref() == Some("retrying") {
                retrying = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if !retrying {
            return Err(format!(
                "a hung door still looked {:?} after 30 s",
                mail_status(native)
            ));
        }
        let returned = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| e.to_string())?;
        *relay.upstream.lock().map_err(|_| "relay")? =
            returned.local_addr().map_err(|e| e.to_string())?;
        relay.hung.store(false, Ordering::SeqCst);
        fake::serve_restarted(returned)
            .await
            .map_err(|e| e.to_string())
    };
    let completed = tokio::time::timeout(std::time::Duration::from_secs(90), async {
        tokio::join!(hang_then_return, driver.run_until(stop))
    })
    .await
    .map_err(|_| "mail never arrived after the door returned")?;
    let calls = completed.0?;
    completed.1?;
    assert!(retrying, "the hang was never shown as retrying");
    assert!(calls.iter().any(|call| call == "subscribe_and_snapshot"));
    let pending = application.lock_inbox().pending_inputs(&session)?;
    assert_eq!(
        pending.len(),
        1,
        "the returned door's mail woke more than once"
    );
    assert_settled_wake_coverage(&application.lock_inbox(), &pending[0])?;
    Ok(())
}

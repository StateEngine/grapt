//! Every HTTP subscription path must identify the window's selected Session.
use super::*;
use std::net::TcpListener;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn subscription_request(reconnect: bool, through_trait: bool) -> TestResult<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let server = std::thread::spawn(move || -> TestResult<String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut peer = loop {
            match listener.accept() {
                Ok((peer, _)) => break peer,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err("window never subscribed".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error.into()),
            }
        };
        peer.set_nonblocking(false)?;
        peer.set_read_timeout(Some(Duration::from_secs(5)))?;
        peer.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut head = Vec::new();
        for _ in 0..MAX_HEAD_BYTES {
            let mut byte = [0];
            peer.read_exact(&mut byte)?;
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")?;
                return Ok(String::from_utf8(head)?);
            }
        }
        Err("window request exceeded the header bound".into())
    });
    let mut connection = HttpConnection::attach(
        address,
        BasicCredential::new("fixture", "DUMMY_AUTH")?,
        Bounds::PRODUCTION,
    );
    let session = Some("ses_window_lease".parse()?);
    if through_trait {
        Connection::watch_session(&mut connection, session);
    } else {
        connection.watch_session(session);
    }
    let subscribed = (|| -> TestResult {
        if !reconnect {
            connection.resubscribe();
            return Ok(());
        }
        let mut result = connection.begin_reconnect()?;
        for _ in 0..5_000 {
            match result {
                Some(true) => return Ok(()),
                Some(false) => return Err("window reconnect failed".into()),
                None => {
                    std::thread::sleep(Duration::from_millis(1));
                    result = connection.poll_reconnect()?;
                }
            }
        }
        Err("window reconnect exceeded its allowance".into())
    })();
    let request = server.join().map_err(|_| "window peer panicked")??;
    subscribed?;
    Ok(request)
}

#[test]
fn generic_window_selection_names_its_http_subscription() -> TestResult {
    let request = subscription_request(false, true)?;
    assert!(
        request.starts_with("GET /api/event?session=ses_window_lease&positions=1 HTTP/1.1\r\n"),
        "generic window selection omitted its lease: {request}"
    );
    Ok(())
}

#[test]
fn incremental_reconnect_keeps_the_http_window_lease() -> TestResult {
    let request = subscription_request(true, false)?;
    assert!(
        request.starts_with("GET /api/event?session=ses_window_lease&positions=1 HTTP/1.1\r\n"),
        "incremental reconnect omitted its lease: {request}"
    );
    Ok(())
}

/// Retained Sessions with long ids still leave the subscribe within the
/// service's query bound: the oldest are dropped, never positions itself.
#[test]
fn long_retained_ids_drop_the_oldest_to_fit_the_query_bound() -> TestResult {
    let mut connection = HttpConnection::attach(
        "127.0.0.1:9".parse()?,
        BasicCredential::new("fixture", "DUMMY_AUTH")?,
        Bounds::PRODUCTION,
    );
    let long = |index: usize| format!("ses_{index:02}{}", "x".repeat(600));
    for index in 0..=grapt_api::u3a::MAX_RETAINED_SESSIONS {
        connection.watch_session(Some(long(index).parse()?));
    }
    let request = connection.subscribe_request()?;
    assert!(
        HttpConnection::encoded_query_bytes(request.query()) <= grapt_api::u3a::MAX_QUERY_BYTES
    );
    let value = |key: &str| {
        request
            .query()
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(value("positions").as_deref(), Some("1"));
    let retained = value("retain").ok_or("no Session was retained")?;
    let kept: Vec<&str> = retained.split(',').collect();
    assert!(kept.len() < grapt_api::u3a::MAX_RETAINED_SESSIONS);
    let newest = long(grapt_api::u3a::MAX_RETAINED_SESSIONS - 1);
    assert_eq!(
        kept.first(),
        Some(&newest.as_str()),
        "the newest left Session is kept first"
    );
    Ok(())
}

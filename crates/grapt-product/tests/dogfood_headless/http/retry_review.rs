//! Retry behavior through the same HTTP helper the product journeys use.
use super::*;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn receive(socket: &mut TcpStream) -> Result<String> {
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut raw = Vec::new();
    loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer)?;
        if count == 0 || raw.len() + count > 8192 {
            return Err("fixture request bound".into());
        }
        raw.extend_from_slice(&buffer[..count]);
        if let Some(end) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let text = std::str::from_utf8(&raw[..end])?;
            let size = text
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .ok_or("length")?
                .parse::<usize>()?;
            if raw.len() >= end + 4 + size {
                return Ok(text.lines().next().ok_or("request")?.to_owned());
            }
        }
    }
}
fn exercise(method: &str, first: u16) -> Result<(bool, Vec<String>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let server = std::thread::spawn(move || -> std::result::Result<Vec<String>, String> {
        let mut seen = Vec::new();
        let began = tokio::time::Instant::now();
        while seen.len() < 2 && began.elapsed() < Duration::from_millis(1500) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket
                        .set_nonblocking(false)
                        .map_err(|error| error.to_string())?;
                    let line = receive(&mut socket).map_err(|error| error.to_string())?;
                    seen.push(line);
                    let status = if seen.len() == 1 { first } else { 200 };
                    // A mutating endpoint may commit before returning an error.
                    // Each request is a separate effect; there is no retry id.
                    write!(socket,"HTTP/1.1 {status} Fixture\r\nRetry-After: 1\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").map_err(|error| error.to_string())?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(seen)
    });
    let accepted = std::panic::catch_unwind(|| json(address, method, "/api/session", "{}")).is_ok();
    let seen = server.join().map_err(|_| "fixture panicked")??;
    Ok((accepted, seen))
}
#[test]
fn review_reader_wait_helper_retries_a_transient_get() -> Result {
    let (accepted, seen) = exercise("GET", 503)?;
    assert!(accepted);
    assert_eq!(seen.len(), 2);
    assert!(seen
        .iter()
        .all(|line| line.starts_with("GET /api/session ")));
    Ok(())
}
#[test]
fn review_reader_wait_helper_preserves_a_nontransient_refusal() -> Result {
    let (accepted, seen) = exercise("GET", 400)?;
    assert!(!accepted);
    assert_eq!(seen.len(), 1);
    Ok(())
}
#[test]
fn review_reader_wait_helper_never_replays_a_mutation_on_a_transient_status() -> Result {
    let (accepted, seen) = exercise("POST", 503)?;
    assert_eq!(
        seen.len(),
        1,
        "a capacity retry sent the mutation twice: {seen:?}; accepted={accepted}"
    );
    assert!(!accepted);
    Ok(())
}

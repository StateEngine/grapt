// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: reuse its bounded callback HTTP admission from
// crates/grapt-mcp-client/src/oauth/callback.rs; retain the OpenAI attribution.
//! Bounded localhost callback HTTP; no received URL or provider text is echoed.
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

impl BrowserSignIn {
    pub(super) async fn callback(&self) -> Result<Zeroizing<String>, SignInError> {
        let port = self
            .listener
            .local_addr()
            .map_err(|_| SignInError::Malformed("browser callback"))?
            .port();
        tokio::time::timeout(SIGN_IN_ALLOWANCE, async {
            for _ in 0..32 {
                let (mut socket, peer) = self
                    .listener
                    .accept()
                    .await
                    .map_err(|_| SignInError::Request("browser callback"))?;
                if !peer.ip().is_loopback() {
                    continue;
                }
                let target =
                    tokio::time::timeout(Duration::from_secs(1), request(&mut socket, port)).await;
                let Ok(Some(target)) = target else {
                    respond(&mut socket, false).await;
                    continue;
                };
                match self.accept(&target) {
                    Ok(Some(code)) => {
                        respond(&mut socket, true).await;
                        return Ok(code);
                    }
                    Ok(None) => respond(&mut socket, false).await,
                    Err(error) => {
                        respond(&mut socket, false).await;
                        return Err(error);
                    }
                }
            }
            Err(SignInError::Malformed("browser callback limit"))
        })
        .await
        .map_err(|_| SignInError::TimedOut)?
    }
}
async fn request(socket: &mut TcpStream, port: u16) -> Option<String> {
    let mut bytes = Vec::with_capacity(1024);
    loop {
        let mut chunk = [0; 1024];
        let count = socket.read(&mut chunk).await.ok()?;
        if count == 0 || bytes.len() + count > 16 * 1024 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            if end + 4 != bytes.len() {
                return None;
            }
            break;
        }
    }
    let head = std::str::from_utf8(&bytes).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    if first.next()? != "GET" {
        return None;
    }
    let target = first.next()?;
    if !target.starts_with('/')
        || target.starts_with("//")
        || target.chars().any(char::is_control)
        || !matches!(first.next()?, "HTTP/1.0" | "HTTP/1.1")
        || first.next().is_some()
    {
        return None;
    }
    let mut host = None;
    for (index, line) in lines.take_while(|line| !line.is_empty()).enumerate() {
        if index >= 64 || line.starts_with([' ', '\t']) {
            return None;
        }
        let (name, value) = line.split_once(':')?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || value.chars().any(char::is_control)
        {
            return None;
        }
        if name.eq_ignore_ascii_case("host") && host.replace(value.trim()).is_some() {
            return None;
        }
        if name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            return None;
        }
    }
    if host != Some(format!("127.0.0.1:{port}").as_str()) {
        return None;
    }
    Some(target.to_owned())
}
async fn respond(socket: &mut TcpStream, accepted: bool) {
    let (status, body) = if accepted {
        ("200 OK", "Authorization received. Return to Grapt.")
    } else {
        (
            "400 Bad Request",
            "Grapt could not accept this authorization callback.",
        )
    };
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\nX-Content-Type-Options: nosniff\r\n\r\n{body}", body.len());
    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        socket.write_all(response.as_bytes()),
    )
    .await;
}

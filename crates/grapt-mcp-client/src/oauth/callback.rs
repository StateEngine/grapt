// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: owned IPv4 loopback listener, bounded attempts/bytes/time,
// exact Host/redirect binding, and no reflected OAuth data in browser responses.

use crate::oauth::{AuthorizationFlow, CodeExchange, OAuthError};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const HEADER_LIMIT: usize = 16 * 1024;
const ATTEMPT_LIMIT: usize = 32;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ListenerError {
    #[error("OAuth loopback listener could not bind or accept a connection")]
    Io,
    #[error("OAuth callback listener has an invalid port or time budget")]
    Configuration,
    #[error("OAuth callback deadline or connection limit was reached")]
    Exhausted,
    #[error("OAuth authorization was declined")]
    Denied,
}

pub struct CallbackListener {
    listener: TcpListener,
    port: u16,
}
impl std::fmt::Debug for CallbackListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackListener")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}
impl CallbackListener {
    /// Bind before registering a client or publishing its authorization URL.
    pub async fn bind() -> Result<Self, ListenerError> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| ListenerError::Io)?;
        let port = listener.local_addr().map_err(|_| ListenerError::Io)?.port();
        Ok(Self { listener, port })
    }
    pub fn port(&self) -> u16 {
        self.port
    }

    pub async fn wait(
        self,
        mut flow: AuthorizationFlow,
        budget: Duration,
    ) -> Result<CodeExchange, ListenerError> {
        if budget.is_zero()
            || budget > Duration::from_secs(120)
            || flow.redirect_uri().host_str() != Some("127.0.0.1")
            || flow.redirect_uri().port() != Some(self.port)
        {
            return Err(ListenerError::Configuration);
        }
        let (exchange, mut stream) = tokio::time::timeout(budget, async {
            for _ in 0..ATTEMPT_LIMIT {
                let (mut stream, peer) = self
                    .listener
                    .accept()
                    .await
                    .map_err(|_| ListenerError::Io)?;
                if !peer.ip().is_loopback() {
                    continue;
                }
                let target = match tokio::time::timeout(
                    Duration::from_secs(1),
                    request_target(&mut stream, self.port),
                )
                .await
                {
                    Ok(Some(target)) => target,
                    _ => {
                        respond(&mut stream, false).await;
                        continue;
                    }
                };
                let raw = format!("http://127.0.0.1:{}{}", self.port, target);
                match flow.accept_callback(&raw) {
                    Ok(exchange) => return Ok((exchange, stream)),
                    Err(OAuthError::Denied) => {
                        respond(&mut stream, false).await;
                        return Err(ListenerError::Denied);
                    }
                    Err(_) => respond(&mut stream, false).await,
                }
            }
            Err(ListenerError::Exhausted)
        })
        .await
        .map_err(|_| ListenerError::Exhausted)??;
        // A valid code is already consumed. Browser I/O cannot make its
        // acceptance deadline discard the exchange before token persistence.
        respond(&mut stream, true).await;
        Ok(exchange)
    }
}

async fn request_target(stream: &mut TcpStream, port: u16) -> Option<String> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let length = stream.read(&mut chunk).await.ok()?;
        if length == 0 || bytes.len().saturating_add(length) > HEADER_LIMIT {
            return None;
        }
        bytes.extend_from_slice(&chunk[..length]);
        if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            if end + 4 != bytes.len() {
                return None;
            }
            break;
        }
    }
    let head = std::str::from_utf8(&bytes).ok()?;
    let mut lines = head.split("\r\n");
    let mut request = lines.next()?.split(' ');
    if request.next()? != "GET" {
        return None;
    }
    let target = request.next()?;
    if !target.starts_with('/') || target.starts_with("//") || target.chars().any(char::is_control)
    {
        return None;
    }
    if !matches!(request.next()?, "HTTP/1.1" | "HTTP/1.0") || request.next().is_some() {
        return None;
    }
    let mut host = None;
    let mut count = 0;
    for line in lines.take_while(|line| !line.is_empty()) {
        count += 1;
        if count > 64 || line.starts_with([' ', '\t']) {
            return None;
        }
        let (name, value) = line.split_once(':')?;
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
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

async fn respond(stream: &mut TcpStream, success: bool) {
    // No authorization URL, code, state, issuer or error text is reflected.
    let (status, body) = if success {
        (
            "200 OK",
            "Grapt authorization received. You may close this window.",
        )
    } else {
        (
            "400 Bad Request",
            "Grapt could not accept this authorization callback.",
        )
    };
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\nX-Content-Type-Options: nosniff\r\n\r\n{body}", body.len());
    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        stream.write_all(response.as_bytes()),
    )
    .await;
}

#[cfg(test)]
#[path = "callback/tests.rs"]
mod tests;

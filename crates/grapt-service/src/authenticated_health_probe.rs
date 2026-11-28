//! One bounded authenticated self-probe for the IA0 product root.

use std::time::Duration;

use grapt_types::service::OwnerReceipt;
use serde::Deserialize;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::ProbeOutcome;

// The product health schema StateEngine also reads; it equals
// grapt_api::product_health::PRODUCT_HEALTH_SCHEMA.
const HEALTH_SCHEMA: &str = "secode.api.health.v1"; // grapt-rename: keep

const MAX_RESPONSE_BYTES: usize = 8_192;
const MAX_BODY_BYTES: usize = 4_096;
const CONNECT_ALLOWANCE: Duration = Duration::from_secs(1);
const IO_ALLOWANCE: Duration = Duration::from_secs(2);
/// The longest one probe can take: its connect, then its request write and
/// its response read, each under its own allowance.
pub const HEALTH_PROBE_ALLOWANCE: Duration = CONNECT_ALLOWANCE
    .saturating_add(IO_ALLOWANCE)
    .saturating_add(IO_ALLOWANCE);

#[derive(Debug, Clone)]
pub struct HealthExpectation {
    owner: OwnerReceipt,
    build: String,
}

impl HealthExpectation {
    pub fn new(owner: OwnerReceipt, build: impl Into<String>) -> Result<Self, HealthProbeRefusal> {
        let build = build.into();
        if build.is_empty() || build.len() > 1_024 {
            return Err(HealthProbeRefusal::InvalidBuild);
        }
        Ok(Self { owner, build })
    }

    pub fn owner(&self) -> &OwnerReceipt {
        &self.owner
    }
}

#[derive(Debug, Clone)]
pub struct AuthenticatedHealthProbe {
    username: String,
    secret: String,
    expected: HealthExpectation,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HealthProbeRefusal {
    #[error("the expected build is empty or exceeds 1024 bytes")]
    InvalidBuild,
    #[error("the Basic identity is empty or exceeds its bound")]
    InvalidCredential,
    #[error("the connect allowance elapsed")]
    ConnectTimeout,
    #[error("the health endpoint could not be connected: {0:?}")]
    Connect(std::io::ErrorKind),
    #[error("the probe request could not be written within its allowance")]
    WriteTimeout,
    #[error("the probe request could not be written: {0:?}")]
    Write(std::io::ErrorKind),
    #[error("the probe response could not be read within its allowance")]
    ReadTimeout,
    #[error("the probe response could not be read: {0:?}")]
    Read(std::io::ErrorKind),
    #[error("the probe response exceeded 8192 bytes")]
    ResponseTooLarge,
    #[error("the probe response was not a complete HTTP/1 response")]
    MalformedHttp,
    #[error("the probe returned status {0}")]
    Status(u16),
    #[error("the probe body exceeded 4096 bytes")]
    BodyTooLarge,
    #[error("the probe body was not the versioned health DTO")]
    InvalidBody,
    #[error("the probe identity did not match pid/build/environment/instance")]
    IdentityMismatch,
}

#[derive(Debug, Deserialize)]
struct WireHealth {
    schema: String,
    pid: u32,
    build: String,
    environment: String,
    service_instance: String,
}

impl AuthenticatedHealthProbe {
    pub fn new(
        username: impl Into<String>,
        secret: impl Into<String>,
        expected: HealthExpectation,
    ) -> Result<Self, HealthProbeRefusal> {
        let username = username.into();
        let secret = secret.into();
        if username.is_empty() || username.len() > 128 || secret.is_empty() || secret.len() > 256 {
            return Err(HealthProbeRefusal::InvalidCredential);
        }
        Ok(Self {
            username,
            secret,
            expected,
        })
    }

    pub async fn probe(&self) -> Result<ProbeOutcome, HealthProbeRefusal> {
        let address = self
            .expected
            .owner
            .endpoint()
            .as_str()
            .strip_prefix("http://")
            .ok_or(HealthProbeRefusal::MalformedHttp)?;
        let mut stream = timeout(CONNECT_ALLOWANCE, TcpStream::connect(address))
            .await
            .map_err(|_| HealthProbeRefusal::ConnectTimeout)?
            .map_err(|error| HealthProbeRefusal::Connect(error.kind()))?;
        let authorization = basic_authorization(&self.username, &self.secret);
        let request = format!(
            "GET /api/health HTTP/1.1\r\nHost: {address}\r\nAuthorization: {authorization}\r\nConnection: close\r\n\r\n"
        );
        timeout(IO_ALLOWANCE, stream.write_all(request.as_bytes()))
            .await
            .map_err(|_| HealthProbeRefusal::WriteTimeout)?
            .map_err(|error| HealthProbeRefusal::Write(error.kind()))?;
        let mut response = Vec::with_capacity(1_024);
        timeout(
            IO_ALLOWANCE,
            stream
                .take((MAX_RESPONSE_BYTES + 1) as u64)
                .read_to_end(&mut response),
        )
        .await
        .map_err(|_| HealthProbeRefusal::ReadTimeout)?
        .map_err(|error| HealthProbeRefusal::Read(error.kind()))?;
        if response.len() > MAX_RESPONSE_BYTES {
            return Err(HealthProbeRefusal::ResponseTooLarge);
        }
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or(HealthProbeRefusal::MalformedHttp)?;
        let head = core::str::from_utf8(&response[..split])
            .map_err(|_| HealthProbeRefusal::MalformedHttp)?;
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or(HealthProbeRefusal::MalformedHttp)?;
        if status != 200 {
            return Err(HealthProbeRefusal::Status(status));
        }
        let body = &response[split + 4..];
        if body.len() > MAX_BODY_BYTES {
            return Err(HealthProbeRefusal::BodyTooLarge);
        }
        let health: WireHealth =
            serde_json::from_slice(body).map_err(|_| HealthProbeRefusal::InvalidBody)?;
        let owner = &self.expected.owner;
        if health.schema != HEALTH_SCHEMA
            || health.pid != owner.pid().value()
            || health.build != self.expected.build
            || health.environment != owner.environment().as_str()
            || health.service_instance != owner.instance().as_str()
        {
            return Err(HealthProbeRefusal::IdentityMismatch);
        }
        Ok(ProbeOutcome::Attributed(owner.clone()))
    }
}

fn basic_authorization(username: &str, secret: &str) -> String {
    let mut plain = String::with_capacity(username.len() + secret.len() + 1);
    plain.push_str(username);
    plain.push(':');
    plain.push_str(secret);
    format!("Basic {}", encode_base64(plain.as_bytes()))
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(a >> 2) as usize] as char);
        out.push(TABLE[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(c & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

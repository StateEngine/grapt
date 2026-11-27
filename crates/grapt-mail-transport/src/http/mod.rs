//! Crate-owned client of the released StateEngine v1 loopback/SSH-tunnel door.
//! No root/seat resolver, sidecar, retry loop or local mailbox lives here.
use crate::operation::{ConnectionFailure, OperationCause, OperationFailure, WriteEffect};
use crate::*;
use grapt_types::{ProtocolVersion, Secret};
use serde_json::{json, Value};
use std::{
    num::NonZeroU16,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
mod decode;
mod failure;
mod operations;
mod page;
mod response;
use response::{Evidence, Response};

/// Private credentials and strict DTO decoding remain behind the sealed port.
/// The port number must name an operator-configured IPv4 loopback listener or
/// SSH tunnel. Redirects and ambient proxies are disabled.
pub struct HttpMailTransport {
    connection: Arc<Connection>,
    binding: Option<crate::binding::MailBindingReceipt>,
    subscription: Option<(
        MailSubscriptionId,
        crate::cursor::SubscriptionGeneration,
        u32,
    )>,
    polling: Arc<AtomicBool>,
    protocol: MailTransportVersion,
}
impl core::fmt::Debug for HttpMailTransport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HttpMailTransport").finish_non_exhaustive()
    }
}
impl HttpMailTransport {
    pub fn new(
        port: NonZeroU16,
        capability: Arc<Secret<String>>,
    ) -> Result<Self, OperationFailure> {
        Self::with_protocol(port, capability, MailTransportVersion::V1)
    }
    pub fn with_protocol(
        port: NonZeroU16,
        capability: Arc<Secret<String>>,
        protocol: MailTransportVersion,
    ) -> Result<Self, OperationFailure> {
        if capability.expose().is_empty()
            || capability.expose().len() > 1024
            || capability.expose().bytes().any(|b| b <= 32 || b >= 127)
        {
            return Err(crate::OpenFailure::Unauthenticated.into());
        }
        let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| unavailable(false))?
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(0)
            .use_preconfigured_tls(config)
            .build()
            .map_err(|_| unavailable(false))?;
        Ok(Self {
            connection: Arc::new(Connection {
                client,
                url: format!("http://127.0.0.1:{port}/{}", protocol.as_str()),
                capability,
            }),
            binding: None,
            subscription: None,
            polling: Arc::new(AtomicBool::new(false)),
            protocol,
        })
    }
    async fn call(
        &self,
        operation: &'static str,
        args: Value,
        ticks: u32,
        bytes: usize,
        write: bool,
    ) -> Result<Response, OperationFailure> {
        self.connection
            .call(operation, args, ticks, bytes, write)
            .await
    }
    fn check(&self, binding: &MailBinding) -> Result<(), OperationFailure> {
        if self.binding.as_ref() != Some(binding.receipt()) {
            return Err(TransportFailure::Disconnected.into());
        }
        Ok(())
    }
}
// A poll owns only the authenticated HTTP connection and a local pending bit;
// it borrows neither the port nor its subscription. Dropping it cancels the
// request and releases the bit. The caller owns its deadline and shutdown.
struct Connection {
    client: reqwest::Client,
    url: String,
    capability: Arc<Secret<String>>,
}
impl Connection {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn call(
        &self,
        operation: &'static str,
        args: Value,
        ticks: u32,
        bytes: usize,
        write: bool,
    ) -> Result<Response, OperationFailure> {
        let body = serde_json::to_vec(&json!({"operation":operation,"args":args}))
            .map_err(|_| malformed(write))?;
        if body.len() > 32768 || ticks == 0 || bytes == 0 {
            return Err(malformed(false));
        }
        let effect = if write {
            WriteEffect::UnknownRetryExact
        } else {
            WriteEffect::None
        };
        let future = async {
            let mut response = self
                .client
                .post(&self.url)
                .header("content-type", "application/json")
                .bearer_auth(self.capability.expose())
                .body(body)
                .send()
                .await
                .map_err(|_| unavailable(write))?;
            let status = response.status();
            let content_type = if response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                == Some("application/json")
            {
                "application/json"
            } else {
                "[absent or invalid]"
            };
            // An intermediary may answer any request with its own status,
            // headers and body. Only a typed JSON refusal speaks for the door;
            // anything else unsuccessful is a lost request, retried as such.
            let lost_poll = operation == "next_change" && !status.is_success();
            let lost_request = !status.is_success() && !status.is_redirection();
            if (lost_poll || lost_request) && content_type != "application/json" {
                grapt_diagnostics::report(format!(
                    "native mail observation unavailable operation={operation} stage=http-status status={}",
                    status.as_u16()
                ));
                return Err(unavailable(write));
            }
            if status.is_redirection() || content_type != "application/json" {
                let result: Result<(), _> = Err(malformed(write));
                Evidence::rejected_headers(
                    operation,
                    status.as_u16(),
                    content_type,
                    self.capability.expose(),
                    &args,
                )
                .observe("headers", &result);
                return Err(malformed(write));
            }
            let mut data = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| unavailable(write))? {
                if chunk.len() > bytes.saturating_sub(data.len()) {
                    data.extend_from_slice(&chunk[..bytes.saturating_sub(data.len())]);
                    let result: Result<(), _> = Err(malformed(write));
                    Evidence::capture(
                        operation,
                        status.as_u16(),
                        content_type,
                        &data,
                        self.capability.expose(),
                        &args,
                    )
                    .observe("body-bound", &result);
                    return Err(malformed(write));
                }
                data.extend_from_slice(&chunk);
            }
            let evidence = Evidence::capture(
                operation,
                status.as_u16(),
                content_type,
                &data,
                self.capability.expose(),
                &args,
            );
            let decoded = (|| {
                let value: Value = decode::strict_value(&data).map_err(|_| malformed(write))?;
                match value.get("ok").and_then(Value::as_bool) {
                    Some(true) => {
                        decode::exact(&value, &["ok", "value"]).map_err(|_| malformed(write))?;
                        if !status.is_success() {
                            return Err(malformed(write));
                        }
                        Ok(value["value"].clone())
                    }
                    Some(false) => {
                        decode::exact(&value, &["ok", "failure"]).map_err(|_| malformed(write))?;
                        Err(failure::decode(&value["failure"], write)
                            .map_err(|_| malformed(write))?)
                    }
                    None => Err(malformed(write)),
                }
            })();
            let decoded = match decoded {
                Err(failure)
                    if lost_poll
                        && failure.cause
                            == OperationCause::Connection(ConnectionFailure::InvalidEnvelope) =>
                {
                    Err(unavailable(false))
                }
                other => other,
            };
            evidence.observe("envelope", &decoded);
            Ok(Response {
                value: decoded?,
                evidence,
            })
        };
        tokio::time::timeout(Duration::from_millis(u64::from(ticks)), future)
            .await
            .map_err(|_| {
                OperationFailure::new(
                    OperationCause::Connection(ConnectionFailure::Deadline),
                    effect,
                )
            })?
    }
}
struct PollLease(Arc<AtomicBool>);
impl Drop for PollLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn malformed(write: bool) -> OperationFailure {
    OperationFailure::new(
        OperationCause::Connection(ConnectionFailure::InvalidEnvelope),
        if write {
            WriteEffect::UnknownRetryExact
        } else {
            WriteEffect::None
        },
    )
}
fn unavailable(write: bool) -> OperationFailure {
    OperationFailure::new(
        OperationCause::Connection(ConnectionFailure::Unavailable),
        if write {
            WriteEffect::UnknownRetryExact
        } else {
            WriteEffect::None
        },
    )
}
impl crate::port::Sealed for HttpMailTransport {}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod wire_tests;

#[cfg(test)]
mod page_tests;

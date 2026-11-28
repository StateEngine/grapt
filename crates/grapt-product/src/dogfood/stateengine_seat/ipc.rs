//! The seat's host over StateEngine's local IPC, with the owner's authority.
//! A connection is opened on first use and dropped after any failure, so the
//! next command rediscovers the endpoint: that covers a host restart.
//! Only Linux can check the connected server today; elsewhere every command
//! refuses with that reason.
use super::config::SeatConfig;
#[cfg(target_os = "linux")]
use super::publisher::Body;
use super::publisher::{SeatHost, SeatSession, SeatTurn};
use grapt_stateengine_ipc::Activity;

pub(super) struct IpcSeatHost {
    config: SeatConfig,
    #[cfg(target_os = "linux")]
    connection: Option<grapt_stateengine_ipc::Connection<tokio::net::UnixStream>>,
}

impl IpcSeatHost {
    pub(super) fn new(config: SeatConfig) -> Self {
        Self {
            config,
            #[cfg(target_os = "linux")]
            connection: None,
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl SeatHost for IpcSeatHost {
    async fn activity(&mut self, _state: Activity) -> Result<SeatSession, String> {
        Err(UNSUPPORTED.to_owned())
    }

    async fn turn(&mut self, _seat: &SeatSession, _turn: &SeatTurn) -> Result<(), String> {
        let _ = &self.config;
        Err(UNSUPPORTED.to_owned())
    }
}

#[cfg(not(target_os = "linux"))]
const UNSUPPORTED: &str =
    "StateEngine seats need a verified host connection, which this platform cannot check yet";

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use grapt_stateengine_ipc::{
        connect, discover, observe_self, verify_binary, Capabilities, Connection, HandshakeOptions,
        IdempotencyKey, PublishTurn, TurnModel, TurnRole, DEFAULT_REPLY_BYTES,
    };
    use std::time::Duration;
    use tokio::net::UnixStream;

    const MAX_IMAGE_BYTES: u64 = 1 << 32;
    /// The bound on one connection or call, so a stalled host costs one report.
    const CALL_BUDGET: Duration = Duration::from_secs(10);
    const OFFERED: [&str; 3] = [
        "ipc.capabilities",
        "session.publish_turn",
        "session.activity",
    ];

    impl IpcSeatHost {
        async fn open(&self) -> Result<Connection<UnixStream>, String> {
            let binding = observe_self(MAX_IMAGE_BYTES).map_err(|error| error.to_string())?;
            let endpoint = discover(&self.config.home, &binding.principal)
                .map_err(|error| error.to_string())?;
            let pin = endpoint.file().image_sha256;
            verify_binary(&self.config.binary, &pin, MAX_IMAGE_BYTES)
                .map_err(|error| error.to_string())?;
            let peer = connect(&endpoint, &binding.principal, &pin, MAX_IMAGE_BYTES)
                .await
                .map_err(|error| error.to_string())?;
            let nonce = grapt_store::OsEntropy::draw_256().map_err(|error| error.to_string())?;
            let options = HandshakeOptions {
                expected_generation: None,
                maximum_reply_bytes: DEFAULT_REPLY_BYTES,
                capabilities: Capabilities::Offer(OFFERED.map(str::to_owned).to_vec()),
            };
            Connection::open(peer, &binding, *nonce.expose(), &options)
                .await
                .map_err(|error| error.to_string())
        }

        /// Run `call` on a live connection within the budget; any failure
        /// drops the connection.
        async fn with<T>(
            &mut self,
            call: impl AsyncFnOnce(&mut Connection<UnixStream>) -> Result<T, String>,
        ) -> Result<T, String> {
            let attempt = async {
                if self.connection.is_none() {
                    self.connection = Some(self.open().await?);
                }
                match self.connection.as_mut() {
                    Some(connection) => call(connection).await,
                    None => Err("the StateEngine connection is unavailable".to_owned()),
                }
            };
            let outcome = tokio::time::timeout(CALL_BUDGET, attempt)
                .await
                .unwrap_or_else(|_| {
                    Err(format!("StateEngine did not answer within {CALL_BUDGET:?}"))
                });
            if outcome.is_err() {
                self.connection = None;
            }
            outcome
        }
    }

    impl SeatHost for IpcSeatHost {
        async fn activity(&mut self, state: Activity) -> Result<SeatSession, String> {
            let (agent, project) = (self.config.agent.clone(), self.config.project.clone());
            self.with(async move |connection| {
                connection
                    .report_activity(&agent, &project, state)
                    .await
                    .map(|receipt| SeatSession {
                        agent: receipt.agent_id,
                        session: receipt.session_id,
                    })
                    .map_err(|error| error.to_string())
            })
            .await
        }

        async fn turn(&mut self, seat: &SeatSession, turn: &SeatTurn) -> Result<(), String> {
            let mut published = PublishTurn {
                idempotency_key: IdempotencyKey(format!("grapt-turn-{}", turn.vendor_turn)),
                target_agent_id: Some(seat.agent.clone()),
                session_id: Some(seat.session.clone()),
                conversation_id: turn.conversation.clone(),
                harness: "grapt".to_owned(),
                model: TurnModel {
                    provider: turn.provider.clone(),
                    model: turn.model.clone(),
                },
                vendor_turn_id: turn.vendor_turn.clone(),
                occurred_at_us: turn.occurred_at_us,
                role: TurnRole::Assistant,
                body_text: None,
                body_omitted_bytes: 0,
                content_sha256: turn.digest,
                usage: None,
                diff_summary: None,
                decision: None,
                nesting: None,
            };
            match &turn.body {
                Body::Whole(text) => published.body_text = Some(text.clone()),
                Body::Omitted(bytes) => published.body_omitted_bytes = *bytes,
            }
            // A body the host's prose bound refuses travels by size alone.
            if let (Err(_), Some(text)) = (published.check(), published.body_text.take()) {
                published.body_omitted_bytes = text.len() as u64;
            }
            let project = self.config.project.clone();
            self.with(async move |connection| {
                connection
                    .publish_turn(&project, &published)
                    .await
                    .map(drop)
                    .map_err(|error| error.to_string())
            })
            .await
        }
    }
}

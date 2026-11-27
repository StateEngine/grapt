// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// at bb64b894fd67158be0f874872e6d7bfe717e7368. Modified for one lifecycle-owned
// GET reader, existing authentication/session headers and bounded backoff.
use crate::{HttpClient, HttpError, ReconnectPolicy};
use std::time::Duration;
use tokio::sync::mpsc;

pub(super) async fn listen(
    client: HttpClient,
    output: mpsc::Sender<Vec<u8>>,
    budget: Duration,
) -> Result<(), HttpError> {
    let mut cursor: Option<String> = None;
    let mut policy = ReconnectPolicy::new();
    let mut lifetime = Duration::ZERO;
    let mut retry = None;
    let mut reconnecting = false;
    loop {
        let mut incoming = match client.listen(cursor.as_deref(), budget).await {
            Ok(incoming) => incoming,
            // The independent notification stream is optional in Streamable HTTP.
            Err(HttpError::Status(405)) => return Ok(()),
            Err(HttpError::Transport) | Err(HttpError::Status(500..=599)) => {
                let delay = policy
                    .next_delay(lifetime, retry)
                    .map_err(|_| HttpError::Transport)?;
                tokio::time::sleep(delay).await;
                lifetime = Duration::ZERO;
                reconnecting = true;
                continue;
            }
            Err(error) => return Err(error),
        };
        if reconnecting {
            for method in [
                "notifications/prompts/list_changed",
                "notifications/resources/list_changed",
            ] {
                output
                    .send(
                        serde_json::to_vec(&serde_json::json!({"jsonrpc":"2.0","method":method}))
                            .map_err(|_| HttpError::Protocol)?,
                    )
                    .await
                    .map_err(|_| HttpError::Transport)?;
            }
        }
        let started = tokio::time::Instant::now();
        loop {
            // Quiet streams have no operation deadline. Dropping the actor
            // aborts this task; every received frame retains decoder bounds.
            let frame = incoming.next_idle().await;
            if let Some(id) = incoming.last_event_id() {
                cursor = (!id.is_empty()).then(|| id.to_owned());
            }
            retry = incoming.retry_delay().or(retry);
            match frame {
                Ok(Some(frame)) => {
                    output.send(frame).await.map_err(|_| HttpError::Transport)?;
                }
                Ok(None)
                | Err(HttpError::Transport)
                | Err(HttpError::Sse(crate::SseDecodeError::Truncated)) => break,
                Err(error) => return Err(error),
            }
        }
        lifetime = started.elapsed();
        let delay = policy
            .next_delay(lifetime, retry)
            .map_err(|_| HttpError::Transport)?;
        tokio::time::sleep(delay).await;
        reconnecting = true;
    }
}

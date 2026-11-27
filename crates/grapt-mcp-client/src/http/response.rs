use super::{HttpClient, HttpError};
use crate::SseDecoder;
use bytes::Bytes;
use reqwest::header::CONTENT_TYPE;
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Json,
    Sse,
}

/// One HTTP response, consumed incrementally. On a read error this handle
/// drops the response; recovery is an explicit GET with the committed cursor.
pub struct Incoming {
    client: HttpClient,
    response: Option<reqwest::Response>,
    encoding: Encoding,
    decoder: SseDecoder,
    chunk: Bytes,
    offset: usize,
}

impl fmt::Debug for Incoming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Incoming")
            .field("encoding", &self.encoding)
            .field("open", &self.response.is_some())
            .finish_non_exhaustive()
    }
}

impl Incoming {
    pub(super) fn new(response: reqwest::Response, client: HttpClient) -> Result<Self, HttpError> {
        let encoding = encoding(&response)?;
        Ok(Self {
            decoder: SseDecoder::new(client.limit)?,
            client,
            response: Some(response),
            encoding,
            chunk: Bytes::new(),
            offset: 0,
        })
    }

    pub(super) fn new_sse(
        response: reqwest::Response,
        client: HttpClient,
    ) -> Result<Self, HttpError> {
        if encoding(&response)? != Encoding::Sse {
            return Err(HttpError::ContentType);
        }
        Self::new(response, client)
    }

    pub fn last_event_id(&self) -> Option<&str> {
        self.decoder.last_event_id()
    }

    pub fn retry_delay(&self) -> Option<Duration> {
        self.decoder.retry_delay()
    }

    /// The whole frame, including comment/metadata dribbles, shares one budget.
    pub async fn next(&mut self, budget: Duration) -> Result<Option<Vec<u8>>, HttpError> {
        if budget.is_zero() {
            self.response = None;
            return Err(HttpError::TimedOut);
        }
        let deadline = self.client.human_wait.deadline(budget)?;
        let result = deadline
            .timeout(self.next_inner())
            .await
            .and_then(|result| result);
        if result.is_err() {
            self.response = None;
            self.chunk = Bytes::new();
            self.offset = 0;
        }
        result
    }

    pub(crate) async fn next_idle(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        self.next_inner().await
    }

    async fn next_inner(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        if self.encoding == Encoding::Json {
            return self.read_json().await;
        }
        loop {
            while let Some(byte) = self.chunk.get(self.offset).copied() {
                self.offset += 1;
                if let Some(event) = self.decoder.push(byte)? {
                    // MCP permits empty cursor-priming events; they are not
                    // JSON-RPC messages and must not enter the protocol parser.
                    if event.data.is_empty() {
                        continue;
                    }
                    if event.event_type != "message" {
                        return Err(HttpError::Protocol);
                    }
                    let data = event.data.into_bytes();
                    self.observe(&data)?;
                    return Ok(Some(data));
                }
            }
            let Some(response) = self.response.as_mut() else {
                return Ok(None);
            };
            match response.chunk().await.map_err(|_| HttpError::Transport)? {
                Some(chunk) => {
                    if chunk.len() > 1024 * 1024 {
                        return Err(HttpError::Oversized);
                    }
                    self.chunk = chunk;
                    self.offset = 0;
                }
                None => {
                    self.response = None;
                    self.chunk = Bytes::new();
                    self.decoder.finish()?;
                    return Ok(None);
                }
            }
        }
    }

    async fn read_json(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        let Some(mut response) = self.response.take() else {
            return Ok(None);
        };
        if response
            .content_length()
            .is_some_and(|length| length > self.client.limit as u64)
        {
            return Err(HttpError::Oversized);
        }
        let mut data = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| HttpError::Transport)? {
            if chunk.len() > self.client.limit.saturating_sub(data.len()) {
                return Err(HttpError::Oversized);
            }
            data.reserve_exact(chunk.len());
            data.extend_from_slice(&chunk);
        }
        self.observe(&data)?;
        Ok(Some(data))
    }

    fn observe(&self, data: &[u8]) -> Result<(), HttpError> {
        self.client.observe_initialize(data)
    }
}

fn encoding(response: &reqwest::Response) -> Result<Encoding, HttpError> {
    match response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
    {
        Some(value) if value.eq_ignore_ascii_case("application/json") => Ok(Encoding::Json),
        Some(value) if value.eq_ignore_ascii_case("text/event-stream") => Ok(Encoding::Sse),
        _ => Err(HttpError::ContentType),
    }
}

//! Select framing before socket reads; native AWS bytes never enter SSE parsing.
use super::{
    eventstream::EventStream,
    sse::{SseDecodeError, SseDecoder},
};
#[derive(Debug)]
pub(crate) enum Framing {
    Sse(SseDecoder),
    EventStream(EventStream),
}
impl Framing {
    pub(crate) fn new(bedrock: bool) -> Self {
        if bedrock {
            Self::EventStream(EventStream::default())
        } else {
            Self::Sse(SseDecoder::new())
        }
    }
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, &'static str> {
        match self {
            Self::Sse(sse) => sse.feed(bytes).map_err(message),
            Self::EventStream(stream) => stream.feed(bytes),
        }
    }
    pub(crate) fn finish(&mut self) -> Result<Vec<String>, &'static str> {
        match self {
            Self::Sse(sse) => sse.finish().map_err(message),
            Self::EventStream(stream) => stream.finish(),
        }
    }
}
fn message(error: SseDecodeError) -> &'static str {
    match error {
        SseDecodeError::NotUtf8 => {
            "the provider stream carried bytes that are not valid UTF-8, or ended mid-character"
        }
        SseDecodeError::Oversized => "the provider stream exceeded the adapter capture bound",
    }
}

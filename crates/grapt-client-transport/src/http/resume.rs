//! Resuming a positions stream (B3 S3b-2): the binding a subscribe is opened
//! under, and the `Last-Event-ID` it carries only when that binding is the one
//! the client's resume token names.
use super::*;
use crate::connection::{ResumeToken, StreamBinding};
use std::hash::{Hash, Hasher};

/// The longest resume id this connection will send.
const MAX_RESUME_ID_BYTES: usize = 128;

impl HttpConnection {
    /// The binding a subscribe opened now would have.
    pub(super) fn binding_for(&self, positions: bool) -> StreamBinding {
        let mut fingerprint = std::hash::DefaultHasher::new();
        self.credential.header_value().hash(&mut fingerprint);
        StreamBinding::new(
            self.address.to_string(),
            fingerprint.finish(),
            self.watched_session().cloned(),
            positions,
        )
    }

    /// The id a subscribe with this query resumes from: the offered token's,
    /// when the subscribe's binding is exactly the token's.
    pub(super) fn resume_header(&self, query: &[(String, String)]) -> Option<&str> {
        let token = self.resume.as_ref()?;
        let positions = query
            .iter()
            .any(|(key, value)| key == "positions" && value == "1");
        (token.binding == self.binding_for(positions)).then_some(token.id.as_str())
    }

    /// Keep a token only when its id is a safe header value.
    pub(super) fn offer_resume(&mut self, token: Option<ResumeToken>) {
        self.resume = token.filter(|token| {
            !token.id.is_empty()
                && token.id.len() <= MAX_RESUME_ID_BYTES
                && token.id.bytes().all(|byte| byte.is_ascii_graphic())
        });
    }

    /// The open stream's binding, recorded when it was installed.
    pub(super) fn open_binding(&self) -> Option<StreamBinding> {
        self.stream.as_ref().and(self.stream_binding.clone())
    }
}

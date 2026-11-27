//! Session management responses share the double’s existing session owner.
use super::*;

impl ContractDouble {
    pub(super) fn delete_session(
        &mut self,
        request: &WireRequest,
    ) -> Result<WireResponse, TransportError> {
        let removed = Self::session_of(request.path()).and_then(|id| self.sessions.remove(&id));
        Ok(WireResponse {
            status: if removed.is_some() { 204 } else { 404 },
            body: Vec::new(),
        })
    }
    pub(super) fn rename_session(
        &mut self,
        request: &WireRequest,
    ) -> Result<WireResponse, TransportError> {
        let Some(session) = Self::session_of(request.path()) else {
            return Ok(WireResponse {
                status: 404,
                body: Vec::new(),
            });
        };
        let Some(bytes) = request.body() else {
            return Ok(WireResponse {
                status: 400,
                body: Vec::new(),
            });
        };
        let Ok(title) = crate::wire::decode_rename(bytes) else {
            return Ok(WireResponse {
                status: 400,
                body: Vec::new(),
            });
        };
        if title.trim().is_empty()
            || title.chars().count() > 100
            || title.chars().any(char::is_control)
        {
            return Ok(WireResponse {
                status: 400,
                body: Vec::new(),
            });
        }
        let Some(entry) = self.sessions.get_mut(&session) else {
            return Ok(WireResponse {
                status: 404,
                body: Vec::new(),
            });
        };
        entry.title = title.trim().to_owned();
        Ok(WireResponse {
            status: 204,
            body: Vec::new(),
        })
    }
}

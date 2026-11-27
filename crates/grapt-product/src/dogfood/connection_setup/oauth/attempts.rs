//! An attempt can be cancelled until it takes the synchronous commit boundary.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum State {
    Pending,
    Finishing,
    Complete,
    Failed,
    Expired,
    Cancelled,
}
pub(super) struct Attempt {
    pub(super) id: IntegrationAttemptIdDto,
    pub(super) user_code: Option<String>,
    recipient: SeatRecipientId,
    created: i64,
    expires: i64,
    pub(super) state: Mutex<State>,
    abort: Mutex<Option<tokio::task::AbortHandle>>,
}
impl Attempt {
    pub(super) fn new(
        id: IntegrationAttemptIdDto,
        recipient: SeatRecipientId,
        created: i64,
        expires: i64,
    ) -> Self {
        Self {
            id,
            user_code: None,
            recipient,
            created,
            expires,
            state: Mutex::new(State::Pending),
            abort: Mutex::new(None),
        }
    }
    pub(super) fn attach(&self, abort: tokio::task::AbortHandle) {
        let state = lock(&self.state);
        if matches!(*state, State::Cancelled | State::Expired) {
            abort.abort();
        }
        *lock(&self.abort) = Some(abort);
    }
    pub(super) fn expire(&self, now: i64) -> bool {
        let mut state = lock(&self.state);
        if now < self.expires || matches!(*state, State::Finishing) {
            return false;
        }
        if matches!(*state, State::Pending) {
            *state = State::Expired;
            if let Some(abort) = lock(&self.abort).as_ref() {
                abort.abort();
            }
        }
        true
    }
    pub(super) fn begin_commit(&self, now: i64) -> bool {
        let mut state = lock(&self.state);
        if !matches!(*state, State::Pending) {
            return false;
        }
        if now >= self.expires {
            *state = State::Expired;
            return false;
        }
        *state = State::Finishing;
        true
    }
    pub(super) fn complete(&self) {
        *lock(&self.state) = State::Complete;
    }
    pub(super) fn fail(&self) {
        let mut state = lock(&self.state);
        if matches!(*state, State::Pending | State::Finishing) {
            *state = State::Failed;
        }
    }
    pub(super) fn cancel(&self) -> bool {
        let mut state = lock(&self.state);
        if !matches!(*state, State::Pending) {
            return false;
        }
        *state = State::Cancelled;
        if let Some(abort) = lock(&self.abort).as_ref() {
            abort.abort();
        }
        true
    }
    pub(super) fn public(&self) -> serde_json::Value {
        let state = lock(&self.state);
        let status = match *state {
            State::Pending | State::Finishing => "pending",
            State::Complete => "complete",
            State::Failed => "failed",
            State::Expired | State::Cancelled => "expired",
        };
        let mut result =
            json!({"status":status,"time":{"created":self.created,"expires":self.expires}});
        if matches!(*state, State::Failed) {
            result["message"] = "ChatGPT sign-in could not finish; try again".into();
        }
        result
    }
}
impl Runtime {
    pub(super) fn find(
        &self,
        principal: &SealedPrincipal,
        integration: &IntegrationIdDto,
        id: &IntegrationAttemptIdDto,
    ) -> Result<Arc<Attempt>, &'static str> {
        if integration.as_str() != "openai" {
            return Err("OAuth attempt not found");
        }
        lock(&self.attempts)
            .iter()
            .find(|attempt| {
                &attempt.id == id && &attempt.recipient == principal.principal().recipient()
            })
            .cloned()
            .ok_or("OAuth attempt not found")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelling_terminal_or_persisting_attempts_preserves_their_result(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let make = || -> Result<Attempt, Box<dyn std::error::Error>> {
            Ok(Attempt::new(
                IntegrationAttemptIdDto::new(DtoString::try_new("oauth-fixture")?),
                SeatRecipientId::try_new("seat_fixture")?,
                0,
                10,
            ))
        };
        let pending = make()?;
        assert!(pending.cancel());
        assert!(!pending.cancel());
        assert!(!pending.begin_commit(1));
        let committing = make()?;
        assert!(committing.begin_commit(1));
        assert!(!committing.cancel());
        assert_eq!(committing.public()["status"], "pending");
        committing.complete();
        assert!(!committing.cancel());
        committing.expire(20);
        assert_eq!(committing.public()["status"], "complete");
        let failed = make()?;
        failed.fail();
        assert!(!failed.cancel());
        assert_eq!(failed.public()["status"], "failed");
        let expired = make()?;
        expired.expire(20);
        assert!(!expired.cancel());
        assert_eq!(expired.public()["status"], "expired");
        Ok(())
    }
}

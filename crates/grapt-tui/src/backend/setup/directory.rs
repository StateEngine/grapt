//! The ordinary directory startup keeps input live while its binding resolves.
use crate::launch::Readiness;
use grapt_client::{Client, ClientError, Connection};
use std::time::Duration;

pub struct Directory {
    directory: String,
    pending: bool,
    finished: bool,
    notice: Option<String>,
}
impl Directory {
    pub fn new(directory: String) -> Self {
        Self {
            directory,
            pending: false,
            finished: false,
            notice: None,
        }
    }
    pub(super) fn cancel<C: Connection>(&mut self, client: &mut Client<C>) {
        client.cancel_directory_offer();
        self.finished = true;
        self.pending = false;
        self.notice = None;
    }

    pub fn retry(&mut self) {
        self.pending = false;
        self.finished = false;
        self.notice = None;
    }
    pub fn wait(&self) -> Option<Duration> {
        (!self.finished).then_some(Duration::from_millis(10))
    }
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    pub fn advance<C: Connection>(&mut self, client: &mut Client<C>) -> Option<Readiness> {
        if self.finished {
            return None;
        }
        let result = (|| -> Result<Option<Readiness>, ClientError> {
            if !client.advance_directory_offer(&self.directory, self.pending)? {
                self.pending = true;
                return Ok(None);
            }
            self.finished = true;
            self.pending = false;
            super::attach(client, None).map(Some)
        })();
        match result {
            Ok(state) => state,
            Err(error) => {
                client.cancel_directory_offer();
                self.finished = true;
                self.pending = false;
                let reason = match error {
                    ClientError::Refused { message, .. } => message,
                    other => other.to_string(),
                };
                let reason: String = reason
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(512)
                    .collect();
                self.notice = Some(format!("Repository metadata unavailable: {reason}"));
                Some(Readiness::Unresolved)
            }
        }
    }
}

#[cfg(test)]
mod tests;

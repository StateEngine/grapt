//! Incremental gateway reads use the same DTO and response checks as other reads.
use super::*;

impl<C: Connection> Client<C> {
    /// Incrementally offer an explicit directory without holding terminal input.
    pub fn advance_directory_offer(
        &mut self,
        directory: &str,
        pending: bool,
    ) -> Result<bool, ClientError> {
        let operation = Operation::ProjectCurrent;
        let response = if pending {
            self.connection.poll_read()
        } else {
            // Host-started I/O never coexists with a pending resume.
            self.abandon_resume();
            let request = WireRequest::new(operation, None, None)?
                .with_query("location[directory]", directory);
            self.connection.begin_read(&request)
        };
        // In flight until it completes, fails or is cancelled.
        self.directory_offer = matches!(response, Ok(None));
        let Some(response) = response? else {
            return Ok(false);
        };
        let bytes = self.accept_response(operation, response)?;
        self.accept_directory(&bytes, directory)?;
        Ok(true)
    }

    pub fn cancel_directory_offer(&mut self) {
        self.directory_offer = false;
        self.connection.cancel_read();
    }
}

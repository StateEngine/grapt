//! Build the durable Session seed a create request describes.
//!
//! Session creation has two responsibilities that change for different reasons:
//! resolving what a request ASKS FOR into a validated seed, and committing that
//! seed. This module is the first. Keeping it separate is what lets a create
//! request grow coordinates — a launch mode today — without the transport
//! handler growing with it.
//!
//! # The launch mode is recorded, never inferred here
//!
//! A client that states how it launched gets that recorded on the seed, and
//! [`create_session`](grapt_session::create_session) writes it durably with the
//! seed. A client that states nothing leaves it absent on purpose: the reader
//! then decides from window presence, which is the older clients' fallback. This
//! module therefore never guesses a mode.

use super::*;

/// Everything a validated create request needs, plus what it asked to record.
pub(super) struct PreparedCreate {
    pub(super) seed: SessionSeed,
}

impl DogfoodApplication {
    /// Resolve a create request into a validated seed.
    ///
    /// Location and model are resolved through the catalog because the request
    /// names them and the caller cannot; agent and id are validated because a
    /// request that names an unusable one must be refused by field, not turned
    /// into a Session nobody can use.
    pub(super) async fn prepare_create(
        &self,
        request: &Value,
        requested_id: Option<SessionId>,
        launch: Option<grapt_types::SessionLaunch>,
    ) -> Result<PreparedCreate, InvalidRequestErrorDto> {
        let location = match request.get("location").filter(|value| !value.is_null()) {
            Some(value) => json!({"location": {
                "directory": value.get("directory"),
                "workspace": value.get("workspaceID"),
            }}),
            None => json!({}),
        };
        let query = decode::<ProjectCurrentLocationQueryDto>(location).map_err(invalid)?;
        let placement = locations::resolve(&self.catalog_snapshot(), &query)
            .await
            .map_err(invalid)?;
        if let Some(agent) = request.get("agent").and_then(Value::as_str) {
            selection::validate_agent(
                self.plugins.service_root(),
                Path::new(&placement.directory),
                agent,
                false,
            )
            .map_err(invalid)?;
        }
        let mut seed = SessionSeed::new(placement.project, placement.directory, "New session")
            .map_err(invalid)?;
        if let Some(id) = requested_id {
            seed = seed.with_requested_id(id).map_err(invalid)?;
        }
        if let Some(agent) = request.get("agent").and_then(Value::as_str) {
            seed = seed.with_agent(agent).map_err(invalid)?;
        }
        if let Some(model) = request.get("model").filter(|value| !value.is_null()) {
            let model = selection::resolve_model(self, model).map_err(invalid)?;
            seed = seed.with_model(model).map_err(invalid)?;
        }
        // A client that stated its launch mode gets it recorded on the Session,
        // so a window that later attaches and leaves cannot stop a Session
        // launched to run alone.
        if let Some(launch) = launch {
            seed = seed.with_launch(launch);
        }
        Ok(PreparedCreate { seed })
    }
}

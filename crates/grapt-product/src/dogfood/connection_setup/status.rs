//! CLI metadata is observed by the same bounded service I/O owner as changes.
use super::*;
impl ConnectionSetup {
    pub(super) fn authentication_status_owned(
        &self,
        principal: SealedPrincipal,
        query: ProjectCurrentLocationQueryDto,
    ) -> ApplicationFuture<AuthenticationStatusResponseDto, InvalidRequestErrorDto> {
        let owner = self.clone();
        Box::pin(async move {
            let _location = owner.location(principal, query).await?;
            let permit = Arc::clone(&owner.operations)
                .try_acquire_owned()
                .map_err(|_| invalid("connections are being changed; retry shortly"))?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let before = Store::read_snapshot(&owner.root).map_err(invalid)?;
                grapt_platform::profile_configuration::ConnectionChange::check_source(
                    &owner.root,
                    before.as_ref(),
                )
                .map_err(invalid)?;
                let rows = grapt_connections::status::read(
                    &owner.root,
                    grapt_store::SystemClock.now(),
                    |name| std::env::var(name).ok(),
                )
                .map_err(invalid)?;
                decode(serde_json::json!({"connections":rows})).map_err(invalid)
            })
            .await
            .map_err(|_| invalid("authentication status owner stopped"))?
        })
    }
}

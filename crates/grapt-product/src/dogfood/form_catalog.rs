//! Pending Session and global forms at a resolved location.
use super::{
    locations,
    support::{decode, invalid},
    DogfoodApplication,
};
use grapt_api::u3a::{
    ApplicationFuture, FormListResponseDto, InvalidRequestErrorDto, ProjectCurrentLocationQueryDto,
};
use grapt_runtime::SessionPorts;
use grapt_types::DeterministicAuthorities;
use serde_json::json;
use std::sync::Arc;

pub(super) fn list(
    application: DogfoodApplication,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<FormListResponseDto, InvalidRequestErrorDto> {
    let inbox = Arc::clone(&application.inbox);
    let ids = Arc::clone(&application.ids);
    let clock = Arc::clone(&application.clock);
    Box::pin(async move {
        let location = locations::resolve_for(&application, &query)
            .await
            .map_err(invalid)?;
        let forms = {
            let held = inbox
                .lock()
                .map_err(|_| invalid("form store lock is poisoned"))?;
            grapt_form::catalog::pending_forms(held.store())
                .map_err(|error| invalid(error.to_string()))?
        };
        let authorities = DeterministicAuthorities::new(&*clock, &*ids);
        let ports = SessionPorts::new(Arc::clone(&inbox), &authorities);
        let mut data = Vec::new();
        let selected = grapt_types::LocationRef::new(&location.directory, None).map_err(invalid)?;
        for form in forms {
            let owner = ports
                .get(&form.session)
                .map_err(|error| invalid(error.to_string()))?;
            if owner.placement.canonical_id() == selected.canonical_id() {
                data.push(form.info);
            }
        }
        let globals = {
            let held = inbox
                .lock()
                .map_err(|_| invalid("form store lock is poisoned"))?;
            grapt_form::global_catalog::pending_global_forms(held.store())
                .map_err(|error| invalid(error.to_string()))?
        };
        for form in globals {
            if form.binding.location.canonical_id() == selected.canonical_id() {
                data.push(super::global_form::projected_info(form));
            }
        }
        decode(json!({"location": location.wire(), "data": data})).map_err(invalid)
    })
}

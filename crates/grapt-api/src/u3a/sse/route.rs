use super::*;

/// Registers the authenticated volatile event stream. A missing hub keeps the
/// route present and returns typed unavailability; it never invents an event.
pub fn register_dogfood_event_route(
    builder: &mut ApiRouterBuilder,
    hub: Arc<DogfoodEventHub>,
) -> Result<(), ApiRefusal> {
    builder.register(
        RouteMethod::Get,
        RoutePath::try_new(DOGFOOD_EVENT_PATH)?,
        move |request: Request<Body>| {
            let hub = Arc::clone(&hub);
            async move {
                if request.extensions().get::<SealedPrincipal>().is_none() {
                    return unauthorized_response();
                }
                let query = match query::event_query(request.uri().query()) {
                    Ok(query) => query,
                    Err(_) => return invalid_request_response("invalid event query"),
                };
                let approver = match Approver::from_request(&request) {
                    Ok(value) => value,
                    Err(()) => return invalid_request_response("invalid approver attachment"),
                };
                let session = query.session.map(|wire| wire.as_inner().clone());
                // An unreadable id is still a resume attempt, refused as malformed.
                let resume = request
                    .headers()
                    .get("last-event-id")
                    .map(|value| value.to_str().unwrap_or_default());
                match hub.subscribe_window(
                    query.location,
                    approver,
                    session,
                    query.positions,
                    resume,
                ) {
                    Ok(body) => sse_response(body),
                    Err(_) => invalid_request_response("event subscription refused"),
                }
            }
        },
    )
}

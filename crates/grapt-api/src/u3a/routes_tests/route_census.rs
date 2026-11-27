use super::*;

#[test]
fn dogfood_routes_preserve_health_and_distinct_session_methods() {
    let max = match MaxRouterModules::try_new(DOGFOOD_NON_SSE_ROUTE_COUNT) {
        Ok(max) => max,
        Err(error) => panic!("route bound failed: {error}"),
    };
    let mut builder = ApiRouterBuilder::new(max);
    if let Err(error) = register_dogfood_routes(&mut builder, never_application()) {
        panic!("Dogfood routes failed: {error}");
    }
    let routes = builder.seal().route_set();
    assert_eq!(routes.len(), DOGFOOD_NON_SSE_ROUTE_COUNT);
    assert!(!routes.iter().any(|route| route.contains("/api/health")));
    assert_eq!(
        routes
            .iter()
            .filter(|route| route.ends_with("/api/session"))
            .count(),
        2,
        "session list and create must remain distinct methods"
    );
}

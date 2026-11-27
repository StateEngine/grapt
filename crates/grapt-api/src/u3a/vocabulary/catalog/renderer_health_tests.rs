use super::*;
use crate::u3a::{ServerConnectedEvent, WireId};
use crate::{ApiRouterBuilder, DogfoodEventHub, MaxRouterModules, ProductHealth, SseLimits};
use hyper::service::Service;
use hyper_util::service::TowerToHyperService;
use std::sync::Arc;

// This projection is private to health compatibility; the shared DTO stays closed.
#[derive(serde::Deserialize)]
struct HealthView {
    healthy: TrueValue,
    version: DtoString,
    pid: PositiveU64,
}

#[tokio::test]
async fn both_served_health_variants_project_through_the_unchanged_closed_dto(
) -> Result<(), Box<dyn std::error::Error>> {
    let identity = ProductHealth::new(41, "attributed-build", "desktop-test", "instance-test")?;
    for reconciles in [false, true] {
        let mut builder = ApiRouterBuilder::new(MaxRouterModules::try_new(1)?);
        if reconciles {
            let hub = Arc::new(DogfoodEventHub::try_new(
                SseLimits::production()?,
                ServerConnectedEvent::new(
                    WireId::parse("evt_renderer_health".to_owned()).ok_or("event identity")?,
                ),
            )?);
            crate::register_product_health_with_reconciles(
                &mut builder,
                identity.clone(),
                hub,
                "product-release",
            )?;
        } else {
            crate::register_product_health(&mut builder, identity.clone(), "product-release")?;
        }
        let service = TowerToHyperService::new(builder.seal().into_router());
        let response = service
            .call(
                axum::http::Request::builder()
                    .uri(crate::PRODUCT_HEALTH_PATH)
                    .body(axum::body::Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(
            response.into_body(),
            crate::product_health::MAX_PRODUCT_HEALTH_BODY_BYTES,
        )
        .await?;
        assert_eq!(serde_json::from_slice::<ProductHealth>(&body)?, identity);
        assert!(
            serde_json::from_slice::<ServiceHealthDto>(&body).is_err(),
            "full attribution must not weaken the closed DTO"
        );
        let view: HealthView = serde_json::from_slice(&body)?;
        let projected = ServiceHealthDto::new(
            Required::new(view.healthy),
            Required::new(view.version),
            Required::new(view.pid),
        );
        let encoded = serde_json::to_value(&projected)?;
        assert_eq!(
            encoded,
            serde_json::json!({"healthy":true,"version":"product-release","pid":41})
        );
        let round_trip: ServiceHealthDto = serde_json::from_value(encoded.clone())?;
        assert_eq!(serde_json::to_value(round_trip)?, encoded);
        let full: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(full.get("global_reconciles").is_some(), reconciles);
        assert_eq!(full["build"], "attributed-build");
    }
    Ok(())
}

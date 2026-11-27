//! The single product-root route admitted by IA0-1R.

use std::sync::Arc;

use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ApiRefusal, ApiRouterBuilder, DogfoodEventHub, ReconcileCounts, RouteMethod, RoutePath,
};
use grapt_stream::replay::ReplayCounts;

pub const PRODUCT_HEALTH_PATH: &str = "/api/health";
// StateEngine reads both receipts and requires these spellings. They change
// only after StateEngine accepts both, in one coordinated step.
pub const PRODUCT_HEALTH_SCHEMA: &str = "secode.api.health.v1"; // grapt-rename: keep
pub const PRODUCT_READY_SCHEMA: &str = "secode.product.ready.v2"; // grapt-rename: keep
pub const MAX_PRODUCT_HEALTH_FIELD_BYTES: usize = 1_024;
pub const MAX_PRODUCT_HEALTH_BODY_BYTES: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductHealth {
    pub schema: String,
    pub pid: u32,
    pub build: String,
    pub environment: String,
    pub service_instance: String,
}

#[derive(Debug, Error)]
pub enum ProductHealthError {
    #[error("health pid must be non-zero")]
    ZeroPid,
    #[error("health field {field} is empty")]
    EmptyField { field: &'static str },
    #[error("health field {field} is {found} bytes; maximum is {maximum}")]
    FieldTooLarge {
        field: &'static str,
        maximum: usize,
        found: usize,
    },
    #[error("health response could not be encoded: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("health response is {found} bytes; maximum is {maximum}")]
    BodyTooLarge { maximum: usize, found: usize },
    #[error("health route was refused: {0}")]
    Route(#[from] ApiRefusal),
}

impl ProductHealth {
    pub fn new(
        pid: u32,
        build: impl Into<String>,
        environment: impl Into<String>,
        service_instance: impl Into<String>,
    ) -> Result<Self, ProductHealthError> {
        if pid == 0 {
            return Err(ProductHealthError::ZeroPid);
        }
        let health = Self {
            schema: PRODUCT_HEALTH_SCHEMA.to_owned(),
            pid,
            build: build.into(),
            environment: environment.into(),
            service_instance: service_instance.into(),
        };
        for (field, value) in [
            ("schema", health.schema.as_str()),
            ("build", health.build.as_str()),
            ("environment", health.environment.as_str()),
            ("service_instance", health.service_instance.as_str()),
        ] {
            if value.is_empty() {
                return Err(ProductHealthError::EmptyField { field });
            }
            if value.len() > MAX_PRODUCT_HEALTH_FIELD_BYTES {
                return Err(ProductHealthError::FieldTooLarge {
                    field,
                    maximum: MAX_PRODUCT_HEALTH_FIELD_BYTES,
                    found: value.len(),
                });
            }
        }
        let _encoded = health.encoded()?;
        Ok(health)
    }

    pub fn encoded(&self) -> Result<String, ProductHealthError> {
        let body = serde_json::to_string(self)?;
        if body.len() > MAX_PRODUCT_HEALTH_BODY_BYTES {
            return Err(ProductHealthError::BodyTooLarge {
                maximum: MAX_PRODUCT_HEALTH_BODY_BYTES,
                found: body.len(),
            });
        }
        Ok(body)
    }
}

pub fn register_product_health(
    builder: &mut ApiRouterBuilder,
    health: ProductHealth,
    version: &str,
) -> Result<(), ProductHealthError> {
    let path = RoutePath::try_new(PRODUCT_HEALTH_PATH)?;
    let body: Arc<str> = Arc::from(desktop_health_body(&health, version, None)?);
    builder.register(RouteMethod::Get, path, move || {
        let body = Arc::clone(&body);
        async move { ([(CONTENT_TYPE, "application/json")], body.to_string()) }
    })?;
    Ok(())
}

/// The existing health route with a live process-local hub snapshot. The identity
/// fields retain their wire shape; clients that only decode ProductHealth ignore
/// the additive desktop and diagnostic fields.
pub fn register_product_health_with_reconciles(
    builder: &mut ApiRouterBuilder,
    health: ProductHealth,
    hub: Arc<DogfoodEventHub>,
    version: &str,
) -> Result<(), ProductHealthError> {
    // Reserve the largest possible fixed snapshot before admitting the route.
    // Later increments cannot make an otherwise valid health body exceed its cap.
    let _largest = hub_health_body(
        &health,
        version,
        Some(ReconcileCounts {
            unpublished_advisory_notice: u64::MAX,
            unrepresentable_frame: u64::MAX,
            projection_failure: u64::MAX,
        }),
        Some(largest_replay_counts()),
    )?;
    let path = RoutePath::try_new(PRODUCT_HEALTH_PATH)?;
    let health = Arc::new(health);
    let version: Arc<str> = Arc::from(version);
    builder.register(RouteMethod::Get, path, move || {
        let health = Arc::clone(&health);
        let hub = Arc::clone(&hub);
        let version = Arc::clone(&version);
        async move {
            let reconciles = Some(hub.reconcile_counts());
            match hub_health_body(&health, &version, reconciles, Some(hub.replay_counts())) {
                Ok(body) => (StatusCode::OK, [(CONTENT_TYPE, "application/json")], body),
                Err(_) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(CONTENT_TYPE, "application/json")],
                    "{\"error\":\"health response could not be encoded\"}".to_owned(),
                ),
            }
        }
    })?;
    Ok(())
}

fn desktop_health_body(
    health: &ProductHealth,
    version: &str,
    global_reconciles: Option<ReconcileCounts>,
) -> Result<String, ProductHealthError> {
    hub_health_body(health, version, global_reconciles, None)
}

/// Every replay-ring count at its largest, for the route's size reservation.
fn largest_replay_counts() -> ReplayCounts {
    ReplayCounts {
        resumed: u64::MAX,
        replayed_frames: u64::MAX,
        evicted_frames: u64::MAX,
        barriers: u64::MAX,
        refused_malformed: u64::MAX,
        refused_epoch: u64::MAX,
        refused_barrier: u64::MAX,
        refused_evicted: u64::MAX,
        refused_ahead: u64::MAX,
        refused_over_batch: u64::MAX,
    }
}

/// The health body with the hub's diagnostics: its reconcile counts and,
/// beside them, its replay-ring counts, so a witness can tell a certified
/// resume from a refused one.
fn hub_health_body(
    health: &ProductHealth,
    version: &str,
    global_reconciles: Option<ReconcileCounts>,
    replays: Option<ReplayCounts>,
) -> Result<String, ProductHealthError> {
    if version.is_empty() {
        return Err(ProductHealthError::EmptyField { field: "version" });
    }
    if version.len() > MAX_PRODUCT_HEALTH_FIELD_BYTES {
        return Err(ProductHealthError::FieldTooLarge {
            field: "version",
            maximum: MAX_PRODUCT_HEALTH_FIELD_BYTES,
            found: version.len(),
        });
    }
    #[derive(Serialize)]
    struct Snapshot<'a> {
        #[serde(flatten)]
        health: &'a ProductHealth,
        healthy: bool,
        version: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        global_reconciles: Option<ReconcileCounts>,
        #[serde(skip_serializing_if = "Option::is_none")]
        replays: Option<ReplayCounts>,
    }
    let body = serde_json::to_string(&Snapshot {
        health,
        healthy: true,
        version,
        global_reconciles,
        replays,
    })?;
    if body.len() > MAX_PRODUCT_HEALTH_BODY_BYTES {
        return Err(ProductHealthError::BodyTooLarge {
            maximum: MAX_PRODUCT_HEALTH_BODY_BYTES,
            found: body.len(),
        });
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MaxRouterModules;

    fn admitted<T, E: core::fmt::Display>(result: Result<T, E>, label: &str) -> T {
        result.unwrap_or_else(|error| panic!("{label} was refused: {error}"))
    }

    #[test]
    fn health_is_versioned_numeric_and_the_only_registered_route() {
        let health = admitted(
            ProductHealth::new(41, "build-a", "internal-alpha", "instance-a"),
            "health DTO",
        );
        let encoded = admitted(health.encoded(), "health encoding");
        let json: serde_json::Value = admitted(serde_json::from_str(&encoded), "health JSON");
        assert_eq!(json["schema"], "secode.api.health.v1");
        assert_eq!(json["pid"], 41);
        assert!(json["pid"].is_number(), "pid must remain numeric");

        let mut builder =
            ApiRouterBuilder::new(admitted(MaxRouterModules::try_new(1), "route bound"));
        admitted(
            register_product_health(&mut builder, health, "0.1.0"),
            "health route",
        );
        let routes = builder.seal().route_set();
        assert_eq!(routes, vec!["GET /api/health"]);
        assert!(
            routes.iter().all(|route| !route.contains("/global/health")),
            "the retired global route must stay absent"
        );
    }

    #[test]
    fn desktop_health_keeps_the_field_and_encoded_body_bounds() -> Result<(), ProductHealthError> {
        let health = ProductHealth::new(
            u32::MAX,
            "b".repeat(1024),
            "e".repeat(1024),
            "i".repeat(1024),
        )?;
        let largest = ReconcileCounts {
            unpublished_advisory_notice: u64::MAX,
            unrepresentable_frame: u64::MAX,
            projection_failure: u64::MAX,
        };
        for counts in [None, Some(largest)] {
            let body = desktop_health_body(&health, "0.1.0", counts)?;
            assert!(body.len() <= MAX_PRODUCT_HEALTH_BODY_BYTES);
            assert_eq!(serde_json::from_str::<ProductHealth>(&body)?, health);
            assert!(matches!(
                desktop_health_body(&health, "", counts),
                Err(ProductHealthError::EmptyField { field: "version" })
            ));
            assert!(matches!(
                desktop_health_body(&health, &"v".repeat(1025), counts),
                Err(ProductHealthError::FieldTooLarge {
                    field: "version",
                    ..
                })
            ));
            assert!(matches!(
                desktop_health_body(&health, &"v".repeat(1024), counts),
                Err(ProductHealthError::BodyTooLarge { .. })
            ));
            assert!(matches!(
                desktop_health_body(&health, &"\"".repeat(600), counts),
                Err(ProductHealthError::BodyTooLarge { .. })
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn health_reads_live_reason_counts_without_changing_identity_or_routes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::u3a::{ServerConnectedEvent, WireId};
        use crate::{ReconcileReason, SseLimits};
        use hyper::service::Service;
        use hyper_util::service::TowerToHyperService;

        let health = ProductHealth::new(41, "build-a", "internal-alpha", "instance-a")?;
        let hub = Arc::new(DogfoodEventHub::try_new(
            SseLimits::production()?,
            ServerConnectedEvent::new(
                WireId::parse("evt_health".to_owned()).ok_or("event identity")?,
            ),
        )?);
        let mut builder = ApiRouterBuilder::new(MaxRouterModules::try_new(1)?);
        register_product_health_with_reconciles(
            &mut builder,
            health.clone(),
            Arc::clone(&hub),
            "0.1.0",
        )?;
        let sealed = builder.seal();
        assert_eq!(sealed.route_set(), vec!["GET /api/health"]);
        let service = TowerToHyperService::new(sealed.into_router());
        let mut snapshots = Vec::with_capacity(4);
        for phase in 0..4 {
            match phase {
                1 => hub.reconcile_because(ReconcileReason::UnpublishedAdvisoryNotice, "notice"),
                2 => hub.reconcile_because(ReconcileReason::UnrepresentableFrame, "bound"),
                3 => hub.invalidate_because("projection"),
                _ => {}
            }
            let request = axum::http::Request::builder()
                .uri(PRODUCT_HEALTH_PATH)
                .body(axum::body::Body::empty())?;
            let response = service.call(request).await?;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes =
                axum::body::to_bytes(response.into_body(), MAX_PRODUCT_HEALTH_BODY_BYTES).await?;
            // Existing readers can still decode and compare the identity-only DTO.
            assert_eq!(serde_json::from_slice::<ProductHealth>(&bytes)?, health);
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            snapshots.push(value["global_reconciles"].clone());
        }
        assert_eq!(
            snapshots,
            vec![
                serde_json::json!({"unpublished_advisory_notice":0,"unrepresentable_frame":0,"projection_failure":0}),
                serde_json::json!({"unpublished_advisory_notice":1,"unrepresentable_frame":0,"projection_failure":0}),
                serde_json::json!({"unpublished_advisory_notice":1,"unrepresentable_frame":1,"projection_failure":0}),
                serde_json::json!({"unpublished_advisory_notice":1,"unrepresentable_frame":1,"projection_failure":1}),
            ]
        );
        Ok(())
    }
}

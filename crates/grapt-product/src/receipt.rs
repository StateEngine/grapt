//! Launch and refusal receipts the server prints on its private stdout protocol.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct LaunchReceipt {
    pub schema: &'static str,
    pub outcome: &'static str,
    pub pid: u32,
    pub endpoint: String,
    pub build: String,
    pub environment: String,
    pub service_instance: String,
    pub registration_released: bool,
    /// Present only when this launch was an explicitly invoked handover.
    ///
    /// Absent on ordinary startup, so an operator reading a receipt can tell
    /// which path produced it rather than inferring it from the invocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handover: Option<HandoverEvidence>,
}

/// What a completed handover proves, carried in the launch receipt.
///
/// `observed_source` is the fingerprint read AFTER the incumbent was observed
/// absent — the value the transition was actually validated against. It is
/// reported rather than merely used, so the freshness of the check is visible
/// to whoever reads the receipt instead of being taken on trust.
#[derive(Debug, Serialize)]
pub struct HandoverEvidence {
    pub schema: &'static str,
    pub serving: String,
    pub observed_source: String,
    pub retired_incumbent: bool,
    /// The controller's own classification of the compare-and-swap.
    pub switch_outcome: String,
}

impl LaunchReceipt {
    pub fn encoded(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[derive(Debug, Serialize)]
struct RefusalReceipt<'a> {
    schema: &'static str,
    outcome: &'static str,
    reason: &'a str,
}

pub(crate) fn refuse(reason: &str) -> i32 {
    refuse_as("grapt.product.launch.v1", "product launch", reason)
}

/// A refusal carries the schema and label of the surface that refused.
pub(crate) fn refuse_as(schema: &'static str, label: &str, reason: &str) -> i32 {
    let receipt = RefusalReceipt {
        schema,
        outcome: "refused",
        reason,
    };
    match serde_json::to_string(&receipt) {
        Ok(encoded) => println!("{encoded}"),
        Err(_) => println!("{{\"schema\":\"{schema}\",\"outcome\":\"refused\"}}"),
    }
    crate::server_log::error(format_args!("{label} refused: {reason}"));
    2
}

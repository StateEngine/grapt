//! Synthetic discovery for a fixture using a configured loopback provider.
use serde_json::{json, Value};
use std::net::SocketAddr;

/// Publish synthetic discovery and a literal loopback route for a fixture profile.
pub(crate) fn configure(root: &std::path::Path, address: SocketAddr) -> std::io::Result<()> {
    let path = root.join("grapt.jsonc");
    let mut config: Value =
        serde_json::from_slice(&std::fs::read(&path)?).map_err(std::io::Error::other)?;
    let model = config["provider"]["model"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("fixture model absent"))?
        .to_owned();
    config["provider"]["protocol_family"] = json!("openai-responses");
    config["provider"]["endpoint"] = json!(format!("http://{address}"));
    if let Some(object) = config.as_object_mut() {
        object.remove("catalog");
    }
    std::fs::write(
        path,
        serde_json::to_vec(&config).map_err(std::io::Error::other)?,
    )?;
    std::fs::write(
        root.join("account-models.json"),
        serde_json::to_vec(&json!({"data":[{"id":model,"max_model_len":128000}]}))
            .map_err(std::io::Error::other)?,
    )?;
    Ok(())
}

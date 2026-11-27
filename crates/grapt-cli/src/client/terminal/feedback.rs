//! Read only the owned product's bounded existing refusal receipt.

use tokio::io::AsyncReadExt;
use tokio::process::Child;

pub(super) async fn startup(child: &mut Child, fallback: String, prefix: Option<u8>) -> String {
    let Some(stdout) = child.stdout.take() else {
        return fallback;
    };
    let mut bytes: Vec<u8> = prefix.into_iter().collect();
    let capture = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        stdout
            .take(2_049 - bytes.len() as u64)
            .read_to_end(&mut bytes),
    )
    .await;
    if !matches!(capture, Ok(Ok(_))) || bytes.len() > 2_048 {
        return fallback;
    }
    match decode(&bytes) {
        Some(reason) => format!("product startup failed: {reason}"),
        None => fallback,
    }
}

fn decode(bytes: &[u8]) -> Option<String> {
    let receipt: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if receipt.get("schema")?.as_str()? != "grapt.product.launch.v1"
        || receipt.get("outcome")?.as_str()? != "refused"
    {
        return None;
    }
    let reason = receipt.get("reason")?.as_str()?;
    if reason.is_empty() || reason.len() > 512 || reason.chars().any(char::is_control) {
        return None;
    }
    Some(reason.to_owned())
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn frc_terminal_feedback_requires_a_bounded_launch_refusal_receipt() {
        let valid = br#"{"schema":"grapt.product.launch.v1","outcome":"refused","reason":"configured credential is unreadable; repair its read access"}"#;
        assert!(decode(valid).is_some_and(|reason| reason.contains("unreadable")));
        assert!(decode(b"raw provider credential").is_none());
        assert!(decode(br#"{"schema":"other","outcome":"refused","reason":"foreign"}"#).is_none());
        let long = serde_json::json!({"schema":"grapt.product.launch.v1","outcome":"refused","reason":"x".repeat(513)});
        assert!(decode(long.to_string().as_bytes()).is_none());
    }
}

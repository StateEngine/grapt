//! Canonical user image references; pixels remain in verified immutable artifacts.
use super::*;
use grapt_types::{ArtifactDigest, ArtifactId, ArtifactRef};
pub(super) fn metadata(images: &[ArtifactRef]) -> Result<Option<Value>, InboxError> {
    if images.is_empty() {
        return Ok(None);
    }
    if images.len() > 4
        || images.iter().any(|r| {
            r.length() == 0
                || r.length() > 1024 * 1024
                || r.id().as_str().len() > 256
                || r.digest().as_str().len() > 256
        })
    {
        return Err(InboxError::PendingBound("user image references"));
    }
    Ok(Some(
        json!({"userImages":{"version":1,"artifacts":images.iter().map(|r|json!({
        "id":r.id().as_str(),"length":r.length(),"digest":r.digest().as_str()})).collect::<Vec<_>>()}}),
    ))
}
/// Decode only this canonical user metadata shape, failing on corrupt references.
pub fn references(value: Option<&Value>) -> Result<Vec<ArtifactRef>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let root = value
        .as_object()
        .ok_or("user image metadata is not an object")?;
    if !root.contains_key("userImages") {
        return Ok(Vec::new());
    }
    if root
        .keys()
        .any(|key| !matches!(key.as_str(), "userImages" | "origin" | "maxSteps"))
    {
        return Err("user image metadata has unknown fields".into());
    }
    let images = root
        .get("userImages")
        .and_then(Value::as_object)
        .ok_or("user image metadata is missing")?;
    if images.len() != 2 || images.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("user image metadata version is invalid".into());
    }
    let rows = images
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or("user image references are missing")?;
    if rows.is_empty() || rows.len() > 4 {
        return Err("user image count exceeds its bound".into());
    }
    rows.iter()
        .map(|row| {
            let row = row.as_object().ok_or("image reference is not an object")?;
            if row.len() != 3 {
                return Err("image reference has unknown fields".into());
            }
            let string = |name| {
                row.get(name)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && s.len() <= 256)
                    .ok_or("image reference identity is invalid")
            };
            let id = ArtifactId::new(string("id")?).ok_or("image artifact identity is invalid")?;
            let digest =
                ArtifactDigest::new(string("digest")?).ok_or("image artifact digest is invalid")?;
            let length = row
                .get("length")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0 && *n <= 1024 * 1024)
                .ok_or("image reference length exceeds its bound")?;
            Ok(ArtifactRef::new(id, length, digest))
        })
        .collect()
}

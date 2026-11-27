use super::{pricing::PricePolicy, report};
use grapt_session::history::SessionHistoryStore;
use grapt_types::SessionId;
use std::io::{Read, Write};
use std::path::Path;

const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// No startup, writer election, credentials, migration or provider operation.
pub fn run(service: &Path, session: &str, price_path: Option<&Path>) -> Result<String, String> {
    let session = session
        .parse::<SessionId>()
        .map_err(|_| "invalid accounting Session identity")?;
    let prices = price_path.map(read_policy).transpose()?;
    let pinned_digest = super::pricing::pinned_digest();
    let path = service.join("database.sqlite3");
    let metadata = std::fs::metadata(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => "accounting database is absent".to_owned(),
        _ => "accounting database is unreadable".to_owned(),
    })?;
    if !metadata.is_file() {
        return Err("accounting database is not a regular file".into());
    }
    let log = grapt_store::EventLog::open_read_only(&path)
        .map_err(|e| format!("accounting read-only database open failed: {e}"))?;
    let snapshot = log
        .with_read_snapshot(|source| SessionHistoryStore::accounting_snapshot(source, &session))?;
    let report = report(
        snapshot,
        Some(match prices.as_ref() {
            Some((policy, digest)) => (policy, digest.as_str()),
            None => (super::pricing::pinned()?, pinned_digest.as_str()),
        }),
    )?;
    let mut output = BoundedOutput(Vec::new());
    serde_json::to_writer(&mut output, &report)
        .map_err(|_| "accounting output exceeds 16 MiB or cannot be encoded")?;
    String::from_utf8(output.0).map_err(|_| "accounting output encoding failed".into())
}

fn read_policy(path: &Path) -> Result<(PricePolicy, String), String> {
    let metadata = std::fs::metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => "price policy is absent",
        _ => "price policy is unreadable",
    })?;
    // Checked before opening: opening a FIFO would block on its absent writer.
    if !metadata.is_file() {
        return Err("price policy must be a regular file".into());
    }
    let file = std::fs::File::open(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => "price policy is absent",
        _ => "price policy is unreadable",
    })?;
    // Checked again on the opened handle: the path may have been swapped.
    if !file
        .metadata()
        .map_err(|_| "price policy metadata unreadable")?
        .is_file()
    {
        return Err("price policy must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(super::pricing::MAX_POLICY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "price policy read failed")?;
    let policy = PricePolicy::decode(&bytes)?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    let digest = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    Ok((policy, digest))
}

struct BoundedOutput(Vec<u8>);
impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_OUTPUT_BYTES - self.0.len() {
            return Err(std::io::Error::other("accounting output byte bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lua_output_bound_refuses_before_allocating_over_limit() {
        let mut output = BoundedOutput(Vec::new());
        assert!(output.write_all(&vec![b'x'; MAX_OUTPUT_BYTES]).is_ok());
        assert!(output.write_all(b"x").is_err(), "output bound survived");
        assert_eq!(output.0.len(), MAX_OUTPUT_BYTES);
    }
}

//! Bounded reads of the artifacts a continuation replays: each refuses past
//! the tool-text bound rather than shorten what history recorded.

use super::*;

fn read_bytes(
    artifacts: &FilesystemArtifactStore,
    reference: &grapt_types::ArtifactRef,
    disposition: CaptureDisposition,
    label: &str,
) -> Result<Vec<u8>, String> {
    fetch_bytes(artifacts, reference, disposition, label)?
        .map_err(|error| format!("{label} artifact is unavailable or corrupt: {error}"))
}

/// What a tool output projects when the store cannot produce its artifact. Its
/// terminal still proves the call ended; only the output is lost, so the model
/// is told that, not refused a turn.
pub(super) fn unreadable_output(label: &str) -> String {
    format!("[Harness notice: the recorded {label} is unavailable or corrupt and cannot be shown. The call's effect may have occurred; verify the current state before relying on it.]")
}

/// The outer error refuses the read's bounds; the inner one is the store
/// failing to produce verified bytes.
pub(super) fn fetch_bytes(
    artifacts: &FilesystemArtifactStore,
    reference: &grapt_types::ArtifactRef,
    disposition: CaptureDisposition,
    label: &str,
) -> Result<Result<Vec<u8>, grapt_artifact::FetchError>, String> {
    if !disposition.is_complete() {
        return Err(format!("{label} artifact is truncated"));
    }
    if reference.length() > MAX_CODEX_TOOL_TEXT_BYTES as u64 {
        return Err(format!("{label} artifact exceeds the continuation bound"));
    }
    let budget = ReadBudget::new(MAX_CODEX_TOOL_TEXT_BYTES as u64)
        .ok_or_else(|| "continuation artifact read bound is zero".to_owned())?;
    let mut offset = 0u64;
    let mut bytes = Vec::new();
    loop {
        let page = match artifacts.fetch(reference, offset, budget) {
            Ok(page) => page,
            Err(error) => return Ok(Err(error)),
        };
        if bytes.len().saturating_add(page.bytes().len()) > MAX_CODEX_TOOL_TEXT_BYTES {
            return Err(format!("{label} artifact exceeded the continuation bound"));
        }
        #[cfg(test)]
        super::tests::FETCHED_BYTES.with(|fetched| fetched.set(fetched.get() + page.bytes().len()));
        bytes.extend_from_slice(page.bytes());
        match page.next_offset() {
            Some(next) => offset = next,
            None => break,
        }
    }
    if bytes.len() as u64 != reference.length() {
        return Err(format!(
            "{label} artifact length changed during reconstruction"
        ));
    }
    Ok(Ok(bytes))
}

pub(in crate::dogfood) fn read_text(
    artifacts: &FilesystemArtifactStore,
    reference: &grapt_types::ArtifactRef,
    disposition: CaptureDisposition,
    label: &str,
) -> Result<CodexToolText, String> {
    let bytes = read_bytes(artifacts, reference, disposition, label)?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{label} artifact is not UTF-8"))?;
    CodexToolText::try_new(text).map_err(|error| error.to_string())
}

pub(super) fn read_output_text(
    artifacts: &FilesystemArtifactStore,
    reference: &grapt_types::ArtifactRef,
    disposition: CaptureDisposition,
    label: &str,
) -> Result<CodexToolText, String> {
    OutputReads::new(artifacts).text(reference, disposition, label)
}

/// The output reads for one terminal. A fetch failure yields the unreadable
/// notice as text and is remembered, so the caller reports the output as
/// failed and no structured receipt parser ever sees the notice.
pub(super) struct OutputReads<'a> {
    pub(super) artifacts: &'a FilesystemArtifactStore,
    unreadable: std::cell::Cell<bool>,
}

impl<'a> OutputReads<'a> {
    pub(super) fn new(artifacts: &'a FilesystemArtifactStore) -> Self {
        Self {
            artifacts,
            unreadable: std::cell::Cell::new(false),
        }
    }

    /// Whether any read for this terminal could not be served.
    pub(super) fn unreadable(&self) -> bool {
        self.unreadable.get()
    }

    /// The notice that stands in for an output the store cannot produce.
    pub(super) fn notice(&self, label: &str) -> String {
        self.unreadable.set(true);
        unreadable_output(label)
    }

    pub(super) fn text(
        &self,
        reference: &grapt_types::ArtifactRef,
        disposition: CaptureDisposition,
        label: &str,
    ) -> Result<CodexToolText, String> {
        let bytes = match fetch_bytes(self.artifacts, reference, disposition, label)? {
            Ok(bytes) => bytes,
            Err(_) => return CodexToolText::try_new(self.notice(label)).map_err(|e| e.to_string()),
        };
        output_text(bytes)
    }
}

fn output_text(bytes: Vec<u8>) -> Result<CodexToolText, String> {
    match String::from_utf8(bytes) {
        Ok(text) => CodexToolText::try_new(text).map_err(|error| error.to_string()),
        Err(error) => {
            const MARKER: &str =
                "\n[tool output projection: invalid UTF-8 bytes were replaced explicitly]";
            let lossy = String::from_utf8_lossy(error.as_bytes());
            let budget = MAX_CODEX_TOOL_TEXT_BYTES.saturating_sub(MARKER.len());
            let mut end = lossy.len().min(budget);
            while !lossy.is_char_boundary(end) {
                end = end
                    .checked_sub(1)
                    .ok_or("tool output UTF-8 projection boundary underflow")?;
            }
            let mut text = lossy[..end].to_owned();
            text.push_str(MARKER);
            CodexToolText::try_new(text).map_err(|error| error.to_string())
        }
    }
}

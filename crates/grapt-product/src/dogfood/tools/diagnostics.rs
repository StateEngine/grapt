//! Bounded diagnostic observations of published bytes, after edit receipts commit.
mod config;
mod process;
mod protocol;
use super::*;
pub(super) use config::Settings;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicI32, Ordering};

// Versions are process-local observations, not durable edit or checkpoint identities.
static VERSION: AtomicI32 = AtomicI32::new(0);
#[derive(Clone)]
pub(super) struct Version(pub(super) i32);
#[derive(Clone)]
pub(super) struct Document {
    path: PathBuf,
    uri: url::Url,
    text: String,
    language: String,
    version: Version,
}

pub(super) struct Context<'a> {
    pub root: &'a Path,
    pub service: &'a Path,
    pub credentials: &'a [String],
    pub settings: Result<Settings, String>,
    pub cancellation: &'a ToolCancellation,
    pub process_owner: Option<(&'a Arc<TerminalRegistry>, &'a SessionId)>,
}

pub(super) struct Images(Vec<(PathBuf, Vec<u8>)>, usize);
impl Images {
    pub(super) fn published(
        planned: &coding::patch::PlannedSteps,
        formatted: &BTreeMap<PathBuf, Vec<u8>>,
        settings: &Result<Settings, String>,
    ) -> Self {
        let mut images = Vec::new();
        let mut omitted = 0;
        let mut budget = 4 * 1024 * 1024;
        let Ok(settings) = settings else {
            return Self(images, omitted);
        };
        for (path, content) in planned.postimages(usize::MAX, &[]) {
            if !settings
                .0
                .values()
                .any(|entry| !entry.disabled && matches(path, entry))
            {
                continue;
            }
            let Some(content) = formatted.get(path).map(Vec::as_slice).or(content) else {
                continue;
            };
            if images.len() == 32 || content.len() > budget {
                omitted += 1;
            } else {
                budget -= content.len();
                images.push((path.to_path_buf(), content.to_vec()));
            }
        }
        Self(images, omitted)
    }
}

impl Context<'_> {
    pub(super) async fn annotate(self, mut output: Vec<u8>, images: Images) -> Vec<u8> {
        let feedback = self.observe(images).await;
        if feedback.is_empty() {
            return output;
        }
        let annotate = || -> Result<Vec<u8>, String> {
            let mut result: Value = serde_json::from_slice(&output).map_err(|e| e.to_string())?;
            result
                .as_object_mut()
                .ok_or("edit result is not an object")?
                .insert("diagnostics".into(), feedback.into());
            serde_json::to_vec(&result).map_err(|e| e.to_string())
        };
        match annotate() {
            Ok(result) => result,
            Err(reason) => {
                output.extend_from_slice(format!("\nDiagnostics annotation unavailable after the completed edit: {reason}; do not retry the edit for diagnostics").as_bytes());
                output
            }
        }
    }

    async fn observe(&self, images: Images) -> Vec<Value> {
        let mut feedback = Vec::new();
        let settings = match &self.settings {
            Ok(settings) => settings,
            Err(reason) => return vec![json!({"status":"unavailable","reason":reason})],
        };
        if settings.0.is_empty() {
            return feedback;
        }
        if images.1 != 0 {
            feedback.push(json!({"status":"unavailable","omittedFiles":images.1,"reason":"diagnostics exceed the 32-file/4-MiB call bound"}));
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut attempts = 0;
        let mut budget = 64 * 1024;
        for (name, entry) in &settings.0 {
            if entry.disabled {
                continue;
            }
            let mut documents = Vec::new();
            for (path, bytes) in &images.0 {
                if !matches(path, entry) {
                    continue;
                }
                match document(self.root, path, bytes, entry) {
                    Ok(document) => documents.push(document),
                    Err(reason) => feedback.push(json!({"path":path,"server":name.as_str(),"status":"unavailable","reason":reason})),
                }
            }
            if documents.is_empty() {
                continue;
            }
            let results = if attempts == 4 || tokio::time::Instant::now() >= deadline {
                Err("LSP reached its four-server/10-second call bound; remaining diagnostics unavailable".into())
            } else {
                attempts += 1;
                let server_deadline =
                    deadline.min(tokio::time::Instant::now() + Duration::from_secs(8));
                process::collect(self, entry, &documents, server_deadline).await
            };
            match results {
                Ok(results) => {
                    for (document, mut result) in documents.iter().zip(results) {
                        result["server"] = name.as_str().into();
                        if let Err(reason) =
                            verify(self.root, &document.path, document.text.as_bytes())
                        {
                            result = json!({"path":document.path,"server":name.as_str(),"status":"unavailable","reason":reason});
                        }
                        let size = result.to_string().len();
                        if size > budget {
                            feedback.push(json!({"path":document.path,"server":name.as_str(),"status":"unavailable","reason":"diagnostic feedback exceeds the 64-KiB call bound"}));
                        } else {
                            budget -= size;
                            feedback.push(result);
                        }
                    }
                }
                Err(reason) => {
                    for document in documents {
                        feedback.push(json!({"path":document.path,"server":name.as_str(),"status":"unavailable","reason":reason}));
                    }
                }
            }
        }
        if feedback
            .iter()
            .map(|item| item.to_string().len())
            .sum::<usize>()
            > 64 * 1024
        {
            return vec![
                json!({"status":"unavailable","omittedResults":feedback.len(),"reason":"diagnostic feedback exceeds the 64-KiB call bound"}),
            ];
        }
        feedback
    }
}

fn matches(path: &Path, entry: &config::Entry) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|file| entry.extensions.iter().any(|suffix| file.ends_with(suffix)))
}

fn document(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    entry: &config::Entry,
) -> Result<Document, String> {
    verify(root, path, bytes)?;
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "LSP document is not UTF-8")?
        .to_owned();
    let previous = VERSION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
        .map_err(|_| "LSP document version exhausted; restart the service")?;
    let uri =
        url::Url::from_file_path(root.join(path)).map_err(|_| "LSP file URI cannot be encoded")?;
    Ok(Document {
        path: path.into(),
        uri,
        text,
        language: entry.language_id(path).into(),
        version: Version(previous + 1),
    })
}

fn verify(root: &Path, path: &Path, expected: &[u8]) -> Result<(), String> {
    let requested = path.to_str().ok_or("LSP path is not UTF-8")?;
    let resolved = resolve_existing(root, requested, true)?;
    let mut bytes = Vec::new();
    coding::patch::open_stage(&resolved)
        .map_err(|_| "LSP document is not a readable regular file")?
        .take(FILE_EFFECT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "LSP document read failed")?;
    if bytes != expected {
        return Err("published file changed before diagnostics; observation unavailable".into());
    }
    Ok(())
}

//! Optional formatting of planned edit bytes before synchronous live publication.
//! Commands see disposable copies; only verified replacements enter the receipt.
mod config;
mod process;
mod receipts;
use super::*;
use coding::patch::{FileExecution, PlannedSteps};
pub(super) use config::Settings;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) struct Context<'a> {
    pub root: &'a Path,
    pub service: &'a Path,
    pub credentials: &'a [String],
    pub settings: Result<Settings, String>,
    pub cancellation: &'a ToolCancellation,
    pub process_owner: Option<(&'a Arc<TerminalRegistry>, &'a SessionId)>,
}

pub(super) struct Prepared {
    originals: receipts::Originals,
    outputs: BTreeMap<PathBuf, (config::Name, Vec<u8>)>,
    feedback: Vec<Value>,
}

impl Context<'_> {
    pub(super) async fn prepare(self, planned: &PlannedSteps) -> Prepared {
        let originals = self.preimages(planned);
        let mut prepared = Prepared {
            originals,
            outputs: BTreeMap::new(),
            feedback: Vec::new(),
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut attempts = 0;
        match &self.settings {
            Err(reason) => prepared.feedback.push(json!({"error":reason})),
            Ok(settings) => {
                'files: for (relative, content) in planned.postimages(usize::MAX, &[]) {
                    let Some(before) = content else { continue };
                    for (name, entry) in &settings.0 {
                        if entry.disabled || !matches_extension(relative, entry) {
                            continue;
                        }
                        if attempts == 32
                            || tokio::time::Instant::now() >= deadline
                            || self.cancellation.cancelled_by().is_some()
                        {
                            prepared.feedback.push(json!({"error":"formatting stopped at cancellation or the 32-command/30-second call bound; remaining edit bytes retained"}));
                            break 'files;
                        }
                        attempts += 1;
                        let result =
                            process::format(&self, relative, before, entry, deadline).await;
                        match result {
                            Ok(after) => {
                                prepared
                                    .outputs
                                    .insert(relative.to_path_buf(), (name.clone(), after));
                                break;
                            }
                            Err(reason) => prepared.feedback.push(
                                json!({"path":relative,"formatter":name.as_str(),"error":reason}),
                            ),
                        }
                    }
                }
            }
        }
        prepared
    }
}

impl Prepared {
    /// No await separates live mutation, formatter publication and its receipt.
    pub(super) fn apply(
        mut self,
        root: &Path,
        planned: &PlannedSteps,
        execution: &mut FileExecution,
    ) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut images = BTreeMap::new();
        if execution.output.is_err() {
            return images;
        }
        let edited = planned.postimages(usize::MAX, &[]);
        let mut diff_budget = 48 * 1024;
        for (relative, (name, after)) in self.outputs {
            let result = edited
                .get(relative.as_path())
                .and_then(|bytes| *bytes)
                .ok_or_else(|| "formatter planned postimage absent".to_owned())
                .and_then(|before| publish(root, &relative, before, &after));
            match result {
                Ok(()) => {
                    let mut file =
                        receipts::metadata(&relative, self.originals.get(&relative), &after);
                    let size = file.to_string().len();
                    if size > diff_budget {
                        file = json!({"relativePath":relative,"patchOmitted":true,"countsAvailable":false,"diff":null,"omittedReason":"formatter diff feedback byte bound"});
                    } else {
                        diff_budget -= size;
                    }
                    self.feedback.push(json!({"path":relative,"formatter":name.as_str(),"formatted":true,"changed":edited.get(relative.as_path()).and_then(|bytes| *bytes) != Some(after.as_slice()),"file":file}));
                    images.insert(relative, after);
                }
                Err(reason) => self
                    .feedback
                    .push(json!({"path":relative,"formatter":name.as_str(),"error":reason})),
            }
        }
        if !self.feedback.is_empty() {
            if let Ok(output) = &mut execution.output {
                match receipts::annotate(output, self.feedback, &images) {
                    Ok(bytes) => *output = bytes,
                    Err(reason) => output.extend_from_slice(
                        format!("\nFormatter feedback encoding failed after the edit: {reason}")
                            .as_bytes(),
                    ),
                }
            }
        }
        images
    }
}

fn publish(root: &Path, relative: &Path, before: &[u8], after: &[u8]) -> Result<(), String> {
    let path = relative.to_str().ok_or("formatter path is not UTF-8")?;
    coding::file_effects::PreparedFile::edit(
        root.join(relative),
        path.into(),
        before.to_vec(),
        after.to_vec(),
        Vec::new(),
    )
    .run(root, None)?
    .output?;
    Ok(())
}

fn matches_extension(path: &Path, entry: &config::Entry) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| {
            entry
                .extensions
                .iter()
                .any(|extension| name.ends_with(extension))
        })
}

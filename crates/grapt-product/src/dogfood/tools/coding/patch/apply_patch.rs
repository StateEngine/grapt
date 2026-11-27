//! Copyright 2025 OpenAI.
//! Licensed under Apache-2.0; see licenses/openai-codex.
//! Modified by Grapt: bounded matching and the existing governed patch executor.
//! Text patch parsing and matching through the existing governed patch path.
use super::*;
use grapt_tools_local::patch::{parse_patch, updated, Hunk};

pub(super) mod filesystem;

pub(in crate::dogfood::tools) const DESCRIPTION: &str = "Apply *** Begin Patch ... *** End Patch text with Add/Update/Delete File, optional Move to and @@ contexts. FREEFORM, not JSON; function-only providers use input. Workspace only; ≤ 16 hunks/64 new directories/256 KiB. Adds overwrite; moves may replace destinations. Snapshots rechecked after permission.";

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::dogfood::tools) struct Arguments {
    input: NonEmptyString,
}
impl ToolArguments for Arguments {
    const TOOL_NAME: &'static str = APPLY_PATCH_TOOL;
    fn provider_schema() -> String {
        serde_json::json!({"type":"object","additionalProperties":false,"required":["input"],"properties":{"input":{"type":"string","minLength":1}}}).to_string()
    }
}

pub(in crate::dogfood::tools) fn definition() -> grapt_provider::ToolDefinition {
    let mut definition = super::super::super::catalog::tool_definition::<Arguments>(DESCRIPTION);
    definition.input_format = grapt_provider::ToolInputFormat::Text {
        grammar: grapt_tools_local::patch::GRAMMAR.into(),
    };
    definition
}

pub(in crate::dogfood::tools) fn prepare(
    root: &Path,
    input: &BoundedToolInput,
) -> Result<PreparedEffect, String> {
    prepare_plan(root, plan(root, input)?)
}

pub(super) fn plan(root: &Path, input: &BoundedToolInput) -> Result<PatchPlan, String> {
    let args: Arguments = decode_input(input)?;
    let parsed = parse_patch(args.input.as_str()).map_err(|error| error.to_string())?;
    if parsed.environment_id.is_some() {
        return Err(
            "this workspace has no alternate patch environment; omit Environment ID".into(),
        );
    }
    if parsed.hunks.is_empty() {
        return Err("No files were modified.".into());
    }
    let mut state = filesystem::Planner::new(root);
    for hunk in parsed.hunks {
        match hunk {
            Hunk::Add { path, contents } => state.add(text_path(&path)?, contents)?,
            Hunk::Delete { path } => state.delete(text_path(&path)?)?,
            Hunk::Update {
                path,
                move_path,
                chunks,
            } => {
                let requested = text_path(&path)?;
                let (path, bytes) = state.source(requested)?;
                let before = String::from_utf8(bytes)
                    .map_err(|error| format!("patch source is not UTF-8: {error}"))?;
                let content = updated(&before, requested, &chunks)?;
                if let Some(moved) = move_path {
                    state.move_to(path, before.as_bytes(), text_path(&moved)?, content)?;
                } else {
                    state.update(requested, content)?;
                }
            }
        }
    }
    Ok(PatchPlan(state.finish(), InputFormat::TextPatch))
}
fn text_path(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "patch path is not UTF-8".into())
}
#[cfg(test)]
mod tests {
    mod effects;
}

use super::*;
mod image_failure;

#[cfg(test)]
thread_local! {
    /// History rows rendered from durable state on this thread: what a page
    /// read cost, for the paging witnesses.
    pub(super) static RENDERED_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count one history row rendered from durable state.
pub(super) fn rendered_row() {
    #[cfg(test)]
    RENDERED_ROWS.with(|rows| rows.set(rows.get() + 1));
}

impl DogfoodApplication {
    pub(super) fn assistant_json(
        &self,
        session: &SessionId,
        message: &AssistantMessage,
    ) -> Result<String, String> {
        self.assistant_json_with_tools(session, message, None, false)
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn assistant_json_with_tools(
        &self,
        session: &SessionId,
        message: &AssistantMessage,
        projections: Option<&continuation::ToolProjections>,
        cache_diagnostics: bool,
    ) -> Result<String, String> {
        rendered_row();
        let created_ms = message
            .opened
            .as_ref()
            .map(|committed| committed.created_ms)
            .ok_or_else(|| {
                format!(
                    "assistant message {} has no typed opening commit",
                    message.message.as_str()
                )
            })?;
        let (usage, observed_usage) = usage::history_usage(&message.usage)?;
        let mut request_metadata = if cache_diagnostics {
            self.executor
                .cache_request_metadata(session, &message.attempt)
        } else {
            None
        }
        .unwrap_or_else(|| json!({}));
        request_metadata
            .as_object_mut()
            .ok_or("request metadata is not an object")?
            .insert(
                "estimatedCost".into(),
                crate::usage_accounting::pricing::assistant_cost(message).unwrap_or_else(
                    |_| json!({"status":"unavailable", "reason":"usage-unavailable"}),
                ),
            );
        let request_metadata = super::metadata_bound::bounded(request_metadata);
        let request_metadata = format!(",\"metadata\":{request_metadata}");
        let mut content = Vec::new();
        if let Some(items) = &message.content {
            for held in items.items() {
                if matches!(held.item.kind(), grapt_types::ContentKind::Opaque) {
                    continue;
                }
                let durable_text = self.read_artifact(held.item.payload())?;
                if let grapt_types::ContentKind::Tool { call } = held.item.kind() {
                    let individual;
                    let projection = match projections.and_then(|projections| projections.get(call))
                    {
                        Some(projection) => projection.as_ref().map_err(Clone::clone)?,
                        None => {
                            individual = continuation::tool_projection(
                                &self.inbox,
                                self.executor.artifacts(),
                                session,
                                call,
                            )?;
                            &individual
                        }
                    };
                    let tool_created_ms = held
                        .committed
                        .as_ref()
                        .map(|position| position.created_ms)
                        .ok_or_else(|| format!("tool {call} has no durable commit position"))?;
                    let call =
                        serde_json::to_string(call.as_str()).map_err(|error| error.to_string())?;
                    let name = serde_json::to_string(&projection.name)
                        .map_err(|error| error.to_string())?;
                    let state = tool_state_json(
                        projection,
                        self.executor.artifacts(),
                        message.terminal.is_some(),
                    )?;
                    content.push(format!(
                        "{{\"type\":\"tool\",\"id\":{call},\"name\":{name},\"state\":{state},\"time\":{{\"created\":{tool_created_ms}}}}}"
                    ));
                    continue;
                }
                let public_text = public_content(&durable_text);
                let public_truncated = public_text.len() < durable_text.len();
                let kind = match held.item.kind() {
                    grapt_types::ContentKind::Text => "text",
                    grapt_types::ContentKind::Reasoning => "reasoning",
                    grapt_types::ContentKind::Tool { .. } => {
                        return Err("tool projection did not take the typed tool path".to_owned());
                    }
                    grapt_types::ContentKind::Opaque => {
                        return Err("opaque content reached text projection".into())
                    }
                };
                let text = serde_json::to_string(public_text).map_err(|error| error.to_string())?;
                let capture = match held.disposition {
                    grapt_types::CaptureDisposition::Complete => "complete",
                    grapt_types::CaptureDisposition::TruncatedAtBoundary { .. } => "truncated",
                };
                let state = serde_json::to_string(&json!({
                    "durableCapture": capture,
                    "durableBytes": durable_text.len(),
                    "publicProjection": if public_truncated { "truncated" } else { "complete" },
                    "publicBoundBytes": PUBLIC_CONTENT_BYTES,
                }))
                .map_err(|error| error.to_string())?;
                content.push(format!(
                    "{{\"type\":\"{kind}\",\"text\":{text},\"state\":{state}}}"
                ));
            }
        }
        let (finish, error) = public_terminal(&message.terminal);

        let id =
            serde_json::to_string(message.message.as_str()).map_err(|error| error.to_string())?;
        let (agent, model) = match &message.selection {
            Some(selection) => {
                let mut model = json!({"providerID":selection.provider().as_str(),"id":selection.model().as_str()});
                if let Some(variant) = selection.variant() {
                    model["variant"] = json!(variant.as_str());
                }
                (selection.agent().as_str(), model)
            }
            None => (
                "default",
                json!({"providerID":self.catalog_snapshot().provider,"id":self.catalog_snapshot().model}),
            ),
        };
        let agent = serde_json::to_string(agent).map_err(|error| error.to_string())?;
        let model = serde_json::to_string(&model).map_err(|error| error.to_string())?;

        let tokens = match usage {
            Some(usage) => format!(
                ",\"tokens\":{{\"input\":{}.0,\"output\":{}.0,\"reasoning\":{}.0,\"cache\":{{\"read\":{}.0,\"write\":{}.0}}}}",
                usage.input,
                usage.output,
                usage.reasoning,
                usage.cache_read,
                usage.cache_write,
            ),
            None => String::new(),
        };
        let (finish, error) = terminal_fragments(finish, error)?;
        // V3 records actual settlement separately from preallocated event identity.
        // Legacy v2 retains its original wire behavior: no historic instant is invented.
        let completed = match &message.terminal_committed {
            Some(position) => {
                let observed = message
                    .completed_at
                    .map(|at| at.as_millis())
                    .unwrap_or(position.created_ms);
                format!(",\"completed\":{observed}.0")
            }
            None => String::new(),
        };
        Ok(format!(
            "{{\"type\":\"assistant\",\"id\":{id},\"time\":{{\"created\":{created_ms}.0{completed}}},\"agent\":{agent},\"model\":{model},\"content\":[{}],\"observedUsage\":{observed_usage}{request_metadata}{finish}{tokens}{error}}}",
            content.join(","),
        ))
    }
}

/// Whether a settled call's output is our image receipt: a local image read
/// proven by its read permission or its explicit local identity, or, for an
/// old auto-approved call with neither, by the receipt marker.
fn native_image_receipt(projection: &continuation::ToolProjection) -> bool {
    projection.name == tools::image::TOOL
        && projection.execution == grapt_types::ToolExecution::Local
        && projection.permission_tool.as_ref().map_or_else(
            || projection.image_read.is_some() || tools::image::has_receipt(&projection.output),
            |tool| tool.as_str() == "read",
        )
        && projection.settled
        && projection.error.is_none()
}

/// One bounded public result view for live delivery and history hydration.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn tool_state_json(
    projection: &continuation::ToolProjection,
    artifacts: &grapt_artifact::FilesystemArtifactStore,
    step_terminal: bool,
) -> Result<String, String> {
    const PUBLIC_FAILURE_DETAIL_BYTES: usize = 2 * 1024;
    let failure_source = eligible_public_failure_detail(projection)
        .map(|detail| super::path_display::failure_text(&detail).into_owned());
    let failure_detail = failure_source.as_deref().and_then(|detail| {
        let mut end = detail.len().min(PUBLIC_FAILURE_DETAIL_BYTES);
        while !detail.is_char_boundary(end) {
            end = end.checked_sub(1)?;
        }
        Some(&detail[..end])
    });
    let image_description;
    let execution_receipt = (projection.execution == grapt_types::ToolExecution::Local
        && matches!(
            projection.name.as_str(),
            "shell" | "exec_command" | "write_stdin" | "request_sandbox_access"
        ))
    .then(|| tools::sandbox::project_receipt(&projection.output, PUBLIC_CONTENT_BYTES))
    .flatten()
    .transpose()?;
    let public_output = if native_image_receipt(projection) {
        image_description = tools::image::replay(&projection.output)?.0;
        image_description.as_str()
    } else if projection.error.is_some() {
        failure_detail.unwrap_or("")
    } else if let Some(receipt) = &execution_receipt {
        receipt.as_str()
    } else {
        public_content(&projection.output)
    };
    let input = serde_json::to_string(&projection.input).map_err(|error| error.to_string())?;
    let output = serde_json::to_string(public_output).map_err(|error| error.to_string())?;
    let attachments = projection
        .files
        .iter()
        .map(|file| continuation::files::content(file, artifacts))
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    let attachment_suffix = if attachments.is_empty() {
        String::new()
    } else {
        format!(",{attachments}")
    };
    let error_text = failure_detail
        .map(|detail| {
            serde_json::to_string(detail)
                .map(|detail| format!("{{\"type\":\"text\",\"text\":{detail}}}"))
                .map_err(|error| error.to_string())
        })
        .transpose()?;
    let error_items = error_text
        .into_iter()
        .chain((!attachments.is_empty()).then_some(attachments.clone()))
        .collect::<Vec<_>>()
        .join(",");
    let error_content = if error_items.is_empty() {
        String::new()
    } else {
        format!(",\"content\":[{error_items}]")
    };
    let public_projection = if projection.error.is_some() && failure_detail.is_none() {
        "excluded"
    } else if public_output.len()
        < failure_source
            .as_deref()
            .map_or(projection.output.len(), str::len)
    {
        "truncated"
    } else {
        "complete"
    };
    let mut metadata = json!({
        "durableCapture": "complete",
        "durableBytes": projection.output.len(),
        "publicProjection": public_projection,
        "publicBoundBytes": if projection.error.is_some() {
            PUBLIC_FAILURE_DETAIL_BYTES
        } else {
            PUBLIC_CONTENT_BYTES
        },
    });
    if projection.name == tools::EDIT_TOOL && projection.settled && projection.error.is_none() {
        // A malformed receipt loses only its file list, never the page.
        let receipt = serde_json::from_str::<Value>(&projection.output).ok();
        if let Some(files) = receipt.as_ref().and_then(|receipt| receipt.get("files")) {
            metadata["files"] = files.clone();
        }
    }
    if let Some(reason) = &projection.input_invalid {
        metadata["inputError"] = Value::String(format!("Invalid tool arguments: {reason}"));
    }
    if let Some(progress) = &projection.progress {
        metadata["toolCalls"] =
            serde_json::to_value(progress.calls()).map_err(|error| error.to_string())?;
    }
    let metadata = super::metadata_bound::bounded(metadata);
    let metadata = serde_json::to_string(&metadata).map_err(|error| error.to_string())?;
    let state = match &projection.error {
        Some(message) => {
            let public_error = super::path_display::failure_text(failure_detail.unwrap_or(message));
            let error = serde_json::to_string(
                &json!({"type":"ToolFailed","message":public_error}),
            )
            .map_err(|error| error.to_string())?;
            format!("{{\"status\":\"error\",\"input\":{input},\"error\":{error}{error_content},\"metadata\":{metadata}}}")
        }
        None if projection.settled => format!(
            "{{\"status\":\"completed\",\"input\":{input},\"content\":[{{\"type\":\"text\",\"text\":{output}}}{attachment_suffix}],\"metadata\":{metadata}}}"
        ),
        None if !step_terminal => format!(
            "{{\"status\":\"running\",\"input\":{input},\"metadata\":{metadata}}}"
        ),
        None => {
            // What the user can do next depends on whether the effect can have
            // happened; only an ask that was never granted proves it did not.
            let message = if projection.never_ran {
                "This tool was interrupted before it ran: its permission was never granted, so nothing was changed. Send a message to continue."
            } else {
                "This tool was interrupted before it reported an outcome, so it may have run. Check its effect, then send a message to continue."
            };
            let error = serde_json::to_string(
                &json!({"type":"ToolOutcomeUnavailable","message":message}),
            )
            .map_err(|error| error.to_string())?;
            format!("{{\"status\":\"error\",\"input\":{input},\"error\":{error},\"metadata\":{metadata}}}")
        }
    };
    Ok(state)
}

/// Only failures with durable typed provenance owned by this product are public.
/// A text prefix is content validation, never provenance: an MCP server can
/// return the same bytes but cannot become a builtin tool or mint a typed
/// AuthorityUnavailable terminal through the permission runner.
fn eligible_public_failure_detail(projection: &continuation::ToolProjection) -> Option<String> {
    let generic = projection.error.as_deref()?;
    if tools::codemode_failure::owns(
        &projection.name,
        projection.execution,
        projection
            .permission_tool
            .as_ref()
            .map(|tool| tool.as_str()),
    ) {
        let detail = projection
            .output
            .strip_prefix(generic)
            .and_then(|text| text.strip_prefix('\n'))
            .unwrap_or("");
        return tools::codemode_failure::public(projection.failure_reason?, detail);
    }
    borrowed_public_failure_detail(projection, generic).map(str::to_owned)
}

fn borrowed_public_failure_detail<'a>(
    projection: &'a continuation::ToolProjection,
    generic: &str,
) -> Option<&'a str> {
    let detail = projection
        .output
        .strip_prefix(generic)?
        .strip_prefix('\n')?;
    if matches!(
        projection.failure_reason,
        Some(
            grapt_types::ToolFailureReason::EffectFailed
                | grapt_types::ToolFailureReason::EffectUnknown
                | grapt_types::ToolFailureReason::AuthorityUnavailable
        )
    ) {
        if let Some(detail) = tools::native_failure::public_detail(
            &projection.name,
            projection
                .permission_tool
                .as_ref()
                .map(|tool| tool.as_str()),
            projection.execution == grapt_types::ToolExecution::Local,
            detail,
        ) {
            return Some(detail);
        }
    }
    if projection.name == tools::image::TOOL {
        return image_failure::detail(projection, detail);
    }
    match projection.failure_reason {
        Some(grapt_types::ToolFailureReason::EffectFailed)
            if matches!(projection.name.as_str(),
                tools::WRITE_TOOL | tools::GLOB_TOOL | tools::GREP_TOOL |
                tools::READ_LINES_TOOL | tools::LIST_TOOL | tools::PATCH_TOOL) =>
        {
            Some(detail)
        }
        // Plan can fail before authorization. Publish only its complete fixed
        // validation messages, never an input-bearing provider/MCP suffix.
        Some(grapt_types::ToolFailureReason::EffectFailed)
            if projection.name == "skill" && detail == "tool preparation failed: skill discovery and loading are exclusive; omit id/file to search, or omit query/cursor to load" => { Some(detail) }
        Some(grapt_types::ToolFailureReason::EffectFailed)
            if projection.name == "skill" && projection.execution == grapt_types::ToolExecution::Local
                && projection.permission_tool.as_ref().is_none_or(|tool| tool.as_str() == "skill") =>
        {
            tools::public_missing_skill_detail(detail)
        }
        Some(grapt_types::ToolFailureReason::EffectFailed)
            if projection.name == tools::PLAN_TOOL && matches!(detail,
                "tool preparation failed: at most one plan step may be in_progress"
                | "tool preparation failed: plan requires at most 64 nonempty steps of 1024 bytes and explanation of 4096 bytes"
            ) => { Some(detail) }
        Some(grapt_types::ToolFailureReason::EffectFailed)
            if projection.name == tools::APPLY_PATCH_TOOL && projection.permission_tool
                .as_ref().is_some_and(|tool| tool.as_str() == "patch") => { Some(detail) }
        Some(grapt_types::ToolFailureReason::EffectFailed)
            if projection.name == tools::HISTORY_TOOL
                && detail.starts_with("tool preparation failed: ") =>
        {
            Some(detail)
        }
        Some(grapt_types::ToolFailureReason::AuthorityUnavailable)
            if detail
                == "the permission request was rejected; do not repeat the call unless the decision changes" =>
        {
            Some(detail)
        }
        _ => None,
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn projection(
        name: &str,
        output: String,
        error: &str,
        failure_reason: grapt_types::ToolFailureReason,
    ) -> continuation::ToolProjection {
        continuation::ToolProjection {
            execution: grapt_types::ToolExecution::Local,
            name: name.to_owned(),
            permission_tool: None,
            image_read: None,
            input: json!({}),
            input_invalid: None,
            output,
            error: Some(error.to_owned()),
            failure_reason: Some(failure_reason),
            settled: true,
            never_ran: false,
            progress: None,
            files: Vec::new(),
        }
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn sr_h02_public_failure_detail_is_actionable_bounded_and_selective() -> Result<(), String> {
        let root =
            std::env::temp_dir().join(format!("grapt-sr-public-detail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let artifacts = grapt_artifact::FilesystemArtifactStore::open(
            root.join("artifacts"),
            b"sr-public-detail".to_vec(),
            Box::new(grapt_store::SystemClock),
        )
        .map_err(|error| error.to_string())?;
        let file = artifacts
            .capture(
                std::io::Cursor::new(b"attachment"),
                grapt_artifact::CaptureBound::new(64 * 1024, 64 * 1024)
                    .ok_or("attachment bound")?,
                grapt_artifact::ArtifactKind::CapturedOutput,
                grapt_artifact::Sensitivity::SecretBearing,
            )
            .map_err(|error| error.to_string())?;
        let safe = format!(
            "Tool failed: EffectFailed\ntool preparation failed: history cursor lists calls{}",
            "x".repeat(4_096)
        );
        let mut safe_projection = projection(
            tools::HISTORY_TOOL,
            safe,
            "Tool failed: EffectFailed",
            grapt_types::ToolFailureReason::EffectFailed,
        );
        safe_projection.files.push(continuation::files::FinalFile {
            reference: file.reference().clone(),
            mime: "text/plain".to_owned(),
        });
        let visible = tool_state_json(&safe_projection, &artifacts, true)?;
        serde_json::from_str::<grapt_api::u3a::SessionMessageToolStateErrorDto>(&visible)
            .map_err(|error| format!("desktop-facing tool DTO rejected safe detail: {error}"))?;
        let visible: Value = serde_json::from_str(&visible).map_err(|error| error.to_string())?;
        let detail = visible["content"][0]["text"]
            .as_str()
            .ok_or("public failure detail is absent")?;
        assert!(detail.starts_with("tool preparation failed: history cursor"));
        assert_eq!(
            visible["error"]["message"], detail,
            "retained desktop error.message lost the actionable safe detail"
        );
        assert_eq!(visible["content"][1]["type"], "file");
        assert!(
            detail.len() <= 2 * 1024,
            "public failure detail exceeded its byte bound"
        );
        assert_eq!(visible["metadata"]["publicProjection"], "truncated");
        assert_eq!(visible["metadata"]["publicBoundBytes"], 2 * 1024);
        let path_failure = projection(
            tools::READ_LINES_TOOL,
            "Tool failed: EffectFailed\n".to_owned()
                + r"tool preparation failed: coding path is outside the workspace rooted at \\?\C:\work",
            "Tool failed: EffectFailed",
            grapt_types::ToolFailureReason::EffectFailed,
        );
        let path_view: Value =
            serde_json::from_str(&tool_state_json(&path_failure, &artifacts, true)?)
                .map_err(|error| error.to_string())?;
        assert_eq!(
            path_view["error"]["message"],
            r"tool preparation failed: coding path is outside the workspace rooted at C:\work"
        );
        assert!(path_failure.output.contains(r"\\?\"));
        for unsafe_output in [
            "Tool failed: EffectFailed\nprovider Authorization: Bearer PRIVATE",
            "Tool failed: EffectFailed\ntool preparation failed: Authorization: Bearer PRIVATE",
            "Tool failed: EffectFailed\nthe permission request was rejected; do not repeat the call unless the decision changes PRIVATE",
        ] {
            let excluded = tool_state_json(
                &projection(
                    "mcp.attacker.history",
                    unsafe_output.to_owned(),
                    "Tool failed: EffectFailed",
                    grapt_types::ToolFailureReason::EffectFailed,
                ),
                &artifacts,
                true,
            )?;
            assert!(
                !excluded.contains("PRIVATE"),
                "MCP magic-prefix detail bypassed typed public provenance"
            );
            let excluded: Value =
                serde_json::from_str(&excluded).map_err(|error| error.to_string())?;
            assert!(excluded.get("content").is_none());
            assert_eq!(excluded["error"]["message"], "Tool failed: EffectFailed");
            assert_eq!(excluded["metadata"]["publicProjection"], "excluded");
        }
        let rejection = "Tool failed: AuthorityUnavailable\nthe permission request was rejected; do not repeat the call unless the decision changes";
        let visible = tool_state_json(
            &projection(
                "shell",
                rejection.to_owned(),
                "Tool failed: AuthorityUnavailable",
                grapt_types::ToolFailureReason::AuthorityUnavailable,
            ),
            &artifacts,
            true,
        )?;
        let visible: Value = serde_json::from_str(&visible).map_err(|error| error.to_string())?;
        assert_eq!(
            visible["error"]["message"],
            "the permission request was rejected; do not repeat the call unless the decision changes"
        );
        drop(artifacts);
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn skill_exclusivity_failure_is_public_without_input_or_foreign_suffixes() {
        let detail = "tool preparation failed: skill discovery and loading are exclusive; omit id/file to search, or omit query/cursor to load";
        for (name, text, expected) in [
            ("skill", detail.to_owned(), Some(detail)),
            ("skill", format!("{detail} PRIVATE"), None),
            ("mcp_skill", detail.to_owned(), None),
        ] {
            let failure = projection(
                name,
                format!("Tool failed: EffectFailed\n{text}"),
                "Tool failed: EffectFailed",
                grapt_types::ToolFailureReason::EffectFailed,
            );
            assert_eq!(
                eligible_public_failure_detail(&failure).as_deref(),
                expected
            );
        }
    }

    #[test]
    fn missing_skill_detail_requires_reserved_local_identity_and_complete_safe_shape(
    ) -> Result<(), String> {
        let detail = "Skill not found in the installed catalog. Nearest available skills: skill-creator. Use {} to list skills or query to search; use an exact installed id to load. A repository helper script is not automatically a skill; run its documented command instead.";
        for (name, permission, text, public) in [
            ("skill", Some("skill"), detail.to_owned(), true),
            ("skill", None, detail.to_owned(), true),
            ("skill", Some("mcp_foreign"), detail.to_owned(), false),
            ("mcp_skill", Some("skill"), detail.to_owned(), false),
            ("skill", Some("skill"), format!("{detail} PRIVATE"), false),
        ] {
            let mut failure = projection(
                name,
                format!("Tool failed: EffectFailed\n{text}"),
                "Tool failed: EffectFailed",
                grapt_types::ToolFailureReason::EffectFailed,
            );
            failure.permission_tool = permission
                .map(|tool| grapt_provider::ToolName::new(tool).ok_or("permission tool"))
                .transpose()?;
            assert_eq!(
                eligible_public_failure_detail(&failure).as_deref(),
                public.then_some(detail)
            );
            failure.execution = grapt_types::ToolExecution::ProviderHosted;
            assert_eq!(eligible_public_failure_detail(&failure).as_deref(), None);
        }
        Ok(())
    }

    #[test]
    fn plan_failures_publish_only_complete_validation_messages() {
        let validation = "tool preparation failed: at most one plan step may be in_progress";
        for detail in [
            validation.to_owned(),
            format!("{validation} PRIVATE"),
            "PRIVATE plan input".to_owned(),
        ] {
            let failure = projection(
                tools::PLAN_TOOL,
                format!("Tool failed: EffectFailed\n{detail}"),
                "Tool failed: EffectFailed",
                grapt_types::ToolFailureReason::EffectFailed,
            );
            assert_eq!(
                eligible_public_failure_detail(&failure).as_deref(),
                if detail == validation {
                    Some(validation)
                } else {
                    None
                }
            );
        }
    }

    #[test]
    fn native_patch_details_require_the_owned_permission_action() -> Result<(), String> {
        let mut failure = projection(
            tools::APPLY_PATCH_TOOL,
            "Tool failed: EffectFailed\npatch source changed".into(),
            "Tool failed: EffectFailed",
            grapt_types::ToolFailureReason::EffectFailed,
        );
        for action in [None, Some("apply_patch"), Some("patch")] {
            failure.permission_tool = action
                .map(|name| grapt_provider::ToolName::new(name).ok_or("action"))
                .transpose()?;
            assert_eq!(
                eligible_public_failure_detail(&failure).as_deref(),
                if action == Some("patch") {
                    Some("patch source changed")
                } else {
                    None
                }
            );
        }
        Ok(())
    }

    #[test]
    fn nt_native_tool_failures_retain_detail_without_admitting_foreign_text() {
        for name in [
            tools::GREP_TOOL,
            tools::READ_LINES_TOOL,
            tools::LIST_TOOL,
            tools::PATCH_TOOL,
        ] {
            let owned = projection(
                name,
                "Tool failed: EffectFailed\npatch stale source hash".to_owned(),
                "Tool failed: EffectFailed",
                grapt_types::ToolFailureReason::EffectFailed,
            );
            assert_eq!(
                eligible_public_failure_detail(&owned).as_deref(),
                Some("patch stale source hash")
            );
        }
        let foreign = projection(
            "mcp.attacker.patch",
            "Tool failed: EffectFailed\npatch stale source hash".to_owned(),
            "Tool failed: EffectFailed",
            grapt_types::ToolFailureReason::EffectFailed,
        );
        assert_eq!(eligible_public_failure_detail(&foreign), None);
    }
}

/// The producer path the live failure used: a settled edit receipt whose diff
/// previews, and an active call's nested tool progress, each over the reader's
/// bound. Every generated state must stay readable, name what went, and keep
/// the fields that explain it.
#[cfg(test)]
mod metadata_bound_tests {
    use super::*;

    fn artifact_store(
        name: &str,
    ) -> Result<(std::path::PathBuf, grapt_artifact::FilesystemArtifactStore), String> {
        let root = std::env::temp_dir().join(format!("grapt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let artifacts = grapt_artifact::FilesystemArtifactStore::open(
            root.join("artifacts"),
            b"metadata-bound".to_vec(),
            Box::new(grapt_store::SystemClock),
        )
        .map_err(|error| error.to_string())?;
        Ok((root, artifacts))
    }

    fn edit_receipt(preview: &str) -> String {
        json!({
            "files": [
                {"relativePath": "a.rs", "diff": {"oldText": preview, "newText": preview}},
                {"relativePath": "b.rs", "diff": {"oldText": preview, "newText": preview}},
            ],
        })
        .to_string()
    }

    #[test]
    fn oversized_edit_receipt_is_bounded_and_marked_in_tool_state() -> Result<(), String> {
        let (root, artifacts) = artifact_store("metadata-bound-edit")?;
        let preview = "x".repeat(5_000);
        let state = tool_state_json(
            &continuation::ToolProjection {
                execution: grapt_types::ToolExecution::Local,
                name: tools::EDIT_TOOL.to_owned(),
                permission_tool: None,
                image_read: None,
                input: json!({"path": "a.rs"}),
                input_invalid: None,
                output: edit_receipt(&preview),
                error: None,
                failure_reason: None,
                settled: true,
                never_ran: false,
                progress: None,
                files: Vec::new(),
            },
            &artifacts,
            true,
        )?;
        serde_json::from_str::<grapt_api::u3a::SessionMessageToolStateCompletedDto>(&state)
            .map_err(|error| format!("reader refused the bounded tool state: {error}"))?;
        let state: Value = serde_json::from_str(&state).map_err(|error| error.to_string())?;
        assert!(state["metadata"].get("files").is_none());
        assert_eq!(
            state["metadata"][super::super::metadata_bound::DROPPED]["omitted"][0],
            "files"
        );
        assert_eq!(state["metadata"]["durableCapture"], "complete");
        drop(artifacts);
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn oversized_nested_tool_calls_are_bounded_and_marked_in_tool_state() -> Result<(), String> {
        let (root, artifacts) = artifact_store("metadata-bound-calls")?;
        let calls = (0..40)
            .map(|index| {
                grapt_api::u3a::ToolProgressEntry::new(
                    "read",
                    grapt_api::u3a::ToolProgressStatus::Running,
                )
                .and_then(|entry| entry.with_message(&format!("{} {index}", "d".repeat(400))))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(str::to_owned)?;
        let progress = grapt_api::u3a::ToolProgressMetadata::new(calls).map_err(str::to_owned)?;
        let state = tool_state_json(
            &continuation::ToolProjection {
                execution: grapt_types::ToolExecution::Local,
                name: "read".to_owned(),
                permission_tool: None,
                image_read: None,
                input: json!({}),
                input_invalid: None,
                output: "done".to_owned(),
                error: None,
                failure_reason: None,
                settled: true,
                never_ran: false,
                progress: Some(progress),
                files: Vec::new(),
            },
            &artifacts,
            true,
        )?;
        serde_json::from_str::<grapt_api::u3a::SessionMessageToolStateCompletedDto>(&state)
            .map_err(|error| format!("reader refused the bounded tool state: {error}"))?;
        let state: Value = serde_json::from_str(&state).map_err(|error| error.to_string())?;
        assert!(state["metadata"].get("toolCalls").is_none());
        assert_eq!(
            state["metadata"][super::super::metadata_bound::DROPPED]["omitted"][0],
            "toolCalls"
        );
        assert_eq!(state["status"], "completed");
        drop(artifacts);
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Nothing optional explains an over-budget projection: the tool state
    /// keeps the marker alone, still says why, and stays readable.
    #[test]
    fn an_unexplained_over_budget_projection_keeps_only_the_marker() -> Result<(), String> {
        let (root, artifacts) = artifact_store("metadata-bound-fallback")?;
        let state = tool_state_json(
            &continuation::ToolProjection {
                execution: grapt_types::ToolExecution::Local,
                name: "read".to_owned(),
                permission_tool: None,
                image_read: None,
                input: json!({}),
                input_invalid: Some("r".repeat(9_000)),
                output: "done".to_owned(),
                error: None,
                failure_reason: None,
                settled: true,
                never_ran: false,
                progress: None,
                files: Vec::new(),
            },
            &artifacts,
            true,
        )?;
        serde_json::from_str::<grapt_api::u3a::SessionMessageToolStateCompletedDto>(&state)
            .map_err(|error| format!("reader refused the bounded tool state: {error}"))?;
        let state: Value = serde_json::from_str(&state).map_err(|error| error.to_string())?;
        let marker = &state["metadata"][super::super::metadata_bound::DROPPED];
        assert!(marker["reason"].is_string(), "the marker says why");
        assert_eq!(marker["omitted"], json!([]));
        assert_eq!(state["status"], "completed");
        drop(artifacts);
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }
}

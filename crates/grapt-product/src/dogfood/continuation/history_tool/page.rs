//! One bounded render batch, using the existing per-call decoder owners.
use super::*;
use grapt_store::session_event_queries::batch;

/// Shared-read groups one render batch may use before calls fall back to
/// their own projection.
const MAX_BATCH_GROUPS: usize = 8;

impl ToolProjection {
    /// A render group can contain several tools in each message. Chunk calls,
    /// not messages, so every selected tool uses the same bounded shared path.
    pub(in crate::dogfood) fn read_batches(
        inbox: &Arc<Mutex<Inbox>>,
        artifacts: &FilesystemArtifactStore,
        session: &SessionId,
        targets: &[ToolCallId],
    ) -> ToolProjections {
        targets
            .chunks(batch::TARGETS)
            .flat_map(|targets| Self::read_batch(inbox, artifacts, session, targets.iter()))
            .collect()
    }

    /// Selected projections belong only to this small render batch. Read each
    /// kind once, retaining each target's original decoder, bounds and refusal.
    /// Errors are consumed at the original row, after its own artifact checks.
    ///
    /// A render batch of rows can hold more calls than one shared read
    /// selects, so the calls are read in groups of [`batch::TARGETS`], up to
    /// [`MAX_BATCH_GROUPS`] groups. A call past that is projected on its own,
    /// as every call past the first group was before (SC-HIST-01: on row A
    /// those individual reads were 4 s of one 50-message page).
    pub(in crate::dogfood) fn read_batch<'a>(
        inbox: &Arc<Mutex<Inbox>>,
        artifacts: &FilesystemArtifactStore,
        session: &SessionId,
        targets: impl Iterator<Item = &'a ToolCallId>,
    ) -> ToolProjections {
        let targets: Vec<_> = targets
            .take(batch::TARGETS * MAX_BATCH_GROUPS)
            .cloned()
            .collect();
        let mut projections = HashMap::new();
        for group in targets.chunks(batch::TARGETS) {
            projections.extend(Self::read_group(inbox, artifacts, session, group.to_vec()));
        }
        projections
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn read_group(
        inbox: &Arc<Mutex<Inbox>>,
        artifacts: &FilesystemArtifactStore,
        session: &SessionId,
        targets: Vec<ToolCallId>,
    ) -> ToolProjections {
        if targets.is_empty() {
            return HashMap::new();
        }
        let inbox = lock(inbox);
        let store = inbox.store();
        let mut fault = None;
        let mut pages =
            match SessionHistoryStore::over(store).tool_call_pages_for(session, &targets) {
                Ok(Some(pages)) => pages
                    .into_iter()
                    .map(|page| {
                        page.map_err(|error| {
                            format!("tool continuation history is unreadable: {error}")
                        })
                    })
                    .collect::<Vec<_>>(),
                Ok(None) => return HashMap::new(),
                Err(error) => {
                    let error = format!("tool continuation history is unreadable: {error}");
                    fault = Some(error.clone());
                    targets.iter().map(|_| Err(error.clone())).collect()
                }
            }
            .into_iter();
        let mut selected = |kind, selection| {
            if let Some(error) = &fault {
                return Some(
                    targets
                        .iter()
                        .map(|_| Err(error.clone()))
                        .collect::<Vec<_>>(),
                );
            }
            match batch::read(
                store,
                session,
                kind,
                &targets,
                selection,
                MAX_CONTINUATION_ROWS,
            ) {
                Ok(Some(pages)) => Some(pages.rows.into_iter().map(Ok).collect()),
                Ok(None) => None,
                Err(error) => {
                    fault = Some(error.clone());
                    Some(targets.iter().map(|_| Err(error.clone())).collect())
                }
            }
        };
        let Some(permissions) = selected(
            "permission.asked.2",
            batch::Selection::Call("$.source.callID"),
        ) else {
            return HashMap::new();
        };
        let Some(identities) = selected(
            "session.tool.input.started.1",
            batch::Selection::Call("$.callID"),
        ) else {
            return HashMap::new();
        };
        let Some(hosted_identities) = selected(
            "session.tool.input.started.2",
            batch::Selection::Call("$.callID"),
        ) else {
            return HashMap::new();
        };
        let Some(outputs) = selected("session.tool.terminal.1", batch::Selection::Model) else {
            return HashMap::new();
        };
        let mut permissions = permissions.into_iter();
        let mut identities = identities.into_iter();
        let mut hosted_identities = hosted_identities.into_iter();
        let mut outputs = outputs.into_iter();
        let mut projections = HashMap::new();
        for target in targets {
            let page = pages.next();
            let permission = permissions.next();
            let identity = identities.next();
            let hosted_identity = hosted_identities.next();
            let output = outputs.next();
            let projection = (|| {
                let page = page.ok_or("selected tool-call page is absent")??;
                let permissions = decode_permission_calls(
                    permission.ok_or("selected permission page is absent")??,
                )?;
                let identities = merge_identities(
                    decode_tool_identities(
                        session,
                        identity.ok_or("selected identity page is absent")??,
                        1,
                    )?,
                    decode_tool_identities(
                        session,
                        hosted_identity.ok_or("selected hosted identity page is absent")??,
                        2,
                    )?,
                    MAX_CONTINUATION_ROWS,
                )?;
                let calls = assemble_calls(page, permissions, identities, artifacts, None)?;
                let outputs = assemble_raw_outputs(
                    output
                        .ok_or("selected output page is absent")??
                        .into_iter()
                        .map(|(seq, data)| (seq, StoredTerminal::read(data.as_bytes())))
                        .collect(),
                    artifacts,
                    None,
                    None,
                    false,
                )?;
                finish_projection(store, artifacts, session, &target, calls, outputs)
            })();
            projections.insert(target, projection);
        }
        projections
    }
}

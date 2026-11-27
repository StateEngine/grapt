//! Rebuild the bounded provider request from durable conversation history.
use super::*;
use crate::dogfood::{prompt_images, tools};

impl DurableContinuation {
    pub(super) fn request_context(
        &self,
        prompt: &str,
        continuation_required: bool,
        until: Option<grapt_types::EventSeq>,
        provider_summaries: bool,
    ) -> Result<RequestContext, ContextError> {
        self.read_projection(|reader| {
            reader.request_context_at(prompt, continuation_required, until, provider_summaries)
        })
    }

    /// `provider_summaries`: whether a provider compaction may stand for the
    /// history it covered. SE's own summarizer, which needs text, and a target
    /// that cannot redeem it rebuild from that durable history instead.
    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn request_context_at(
        &self,
        prompt: &str,
        continuation_required: bool,
        until: Option<grapt_types::EventSeq>,
        provider_summaries: bool,
    ) -> Result<RequestContext, ContextError> {
        let inbox = lock(&self.inbox);
        require_resolved_effects(inbox.store(), &self.session)?;
        let retained =
            retained::RetainedContext::load(&inbox, &self.artifacts, &self.session, &self.input)?
                .filter(|context| match &context.summary {
                    retained::RetainedSummary::Provider(items) => {
                        provider_summaries && self.redeems(items)
                    }
                    retained::RetainedSummary::Text(_) => true,
                });
        let from = self.read_from(retained.as_ref());
        let mut users = match (&retained, until) {
            // A summary window starting past the retained cut: the caller
            // chose it within the read bounds, so no descriptor is consulted.
            (_, until) if self.summary_floor().is_some() && from == self.summary_floor() => inbox
                .promoted_messages_floor(
                    &self.session,
                    self.summary_floor()
                        .unwrap_or_else(|| grapt_types::EventSeq::new(0)),
                    until,
                    MAX_CONTINUATION_ROWS,
                ),
            (Some(context), Some(until)) => inbox.promoted_messages_window_from(
                &self.session,
                context.from,
                &context.generation,
                until,
                MAX_CONTINUATION_ROWS,
            ),
            (Some(context), None) => inbox.promoted_messages_from(
                &self.session,
                context.from,
                &context.generation,
                MAX_CONTINUATION_ROWS,
            ),
            (None, Some(until)) => {
                inbox.promoted_messages_window(&self.session, until, MAX_CONTINUATION_ROWS)
            }
            (None, None) => inbox.promoted_messages(&self.session, MAX_CONTINUATION_ROWS),
        }
        .map_err(|error| format!("conversation user history is unreadable: {error}"))?;
        self.include_current_inputs(
            &inbox,
            &mut users,
            retained.as_ref().map(|context| &context.input),
        )?;
        users.sort_by_key(|user| user.seq);
        let current = users
            .iter()
            .position(|user| user.input == self.input)
            .ok_or_else(|| "current prompt has no promoted durable input".to_owned())?;
        if users[current].text != prompt {
            return Err("current prompt differs from its durable input".into());
        }
        // A replay at an earlier input must not acquire a later user's context.
        let ceiling = users
            .get(current + 1)
            .map(|user| user.seq)
            .unwrap_or(u64::MAX);
        let current_input = users[current].seq;
        let users = &users[..=current];
        self.refuse_declared_overflow(&inbox, from, until)?;
        let permissions = permission_calls(inbox.store(), &self.session, from, until)?;
        let calls = stored_calls(inbox.store(), &self.session, &self.artifacts, from, until)?;
        let outputs = stored_outputs(inbox.store(), &self.session, &self.artifacts, from, until)?;
        let reader = SessionHistoryStore::over(inbox.store());
        let history = match (from, until) {
            (Some(from), Some(until)) => reader.fold_window_from(&self.session, from, until),
            (Some(from), None) => reader.fold_from(&self.session, from),
            (None, Some(until)) => reader.fold_window(&self.session, until),
            (None, None) => reader.fold(&self.session),
        }
        .map_err(|error| format!("conversation assistant history is unreadable: {error}"))?;
        let messages = history.messages().map_err(|error| error.to_string())?;
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        let mut reasoning_bytes = 0usize;
        // A summary replaces the history it covers, and reasoning from that
        // history no longer continues anything the request carries.
        let summarized = retained.as_ref().map(|context| context.source_head.value());
        // Forks rebuild their instruction prefix and copied conversation positions.
        // Anthropic signatures bind to the original prefix, so inherited thinking
        // cannot continue it. Execution starts are never copied by the fork owner:
        // the first local start distinguishes inherited from the child's own state.
        let inherited_before = if grapt_session::SessionStore::over(inbox.store())
            .history_fork_origin(&self.session)
            .map_err(|error| error.to_string())?
            != self.session
        {
            let first: Option<i64> = inbox.store().connection().query_row(
                "SELECT (SELECT seq FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
                 AND type = 'session.execution.started.1' ORDER BY seq LIMIT 1)",
                [self.session.as_str()],
                |row| row.get(0),
            ).map_err(|error| error.to_string())?;
            Some(
                first
                    .map(u64::try_from)
                    .transpose()
                    .map_err(|_| "local execution has a negative sequence")?
                    .unwrap_or(u64::MAX),
            )
        } else {
            None
        };
        let child = if inherited_before.is_some() {
            grapt_subagent::SubagentService::child_context_on(inbox.store(), &self.session)
                .map_err(|error| error.to_string())?
        } else {
            None
        };
        // Label the parent's last request in this projection: otherwise native
        // Messages merges that unfinished spawn request with the child's task
        // into one User message, inviting the child to perform it again.
        let parent_input = if child.is_some() {
            // RuntimeApplication::join records the execution start when the
            // first promotion opens its chain, before joining later Steers.
            // Resolve that original input through the Inbox's existing owner;
            // later inputs, restarts and instruction compaction cannot move it.
            let start = inherited_before.ok_or("child has no local execution boundary")?;
            let initial = inbox
                .original_promoted_input_before(&self.session, grapt_types::EventSeq::new(start))
                .map_err(|error| error.to_string())?
                .ok_or("forked child execution has no initial promoted input")?;
            let boundary = inbox
                .promoted_cursor(&self.session, &initial)
                .map_err(|error| error.to_string())?
                .ok_or("forked child's initial input has no promotion position")?;
            users
                .iter()
                .rev()
                .find(|user| {
                    user.seq < boundary && user.kind == grapt_inbox::PromotedInputKind::User
                })
                .map(|user| &user.input)
        } else {
            None
        };
        for user in users {
            let mut item = prompt_images::input(&self.artifacts, user)?;
            if parent_input == Some(&user.input) {
                let labelled = |text: &str| {
                    format!(
                    "Historical user message to the parent session, before this child was created:\n<parent_message>\n{text}\n</parent_message>"
                )
                };
                match &mut item {
                    CodexInputItem::Message(message) => {
                        message.content = labelled(&message.content)
                    }
                    CodexInputItem::ImageUser { text, .. } => {
                        *text = CodexToolText::try_new(labelled(text.as_str()))
                            .map_err(|error| error.to_string())?;
                    }
                    _ => return Err("parent input is not a user message".into()),
                }
            }
            push_context(&mut rows, &mut bytes, user.seq, item)?;
        }
        if let Some(boundary) = inherited_before.filter(|seq| {
            *seq < ceiling
                && from.is_none_or(|from| *seq >= from.value())
                && until.is_none_or(|until| *seq < until.value())
        }) {
            if child.is_some() {
                // Place the role boundary beside the child's first task, after
                // inherited history. Its immutable execution position keeps the
                // child's own signed replay prefix stable on later requests.
                let role = if lock(&self.redeemer).as_ref().is_some_and(|target| {
                    target.family() == grapt_provider::ProtocolFamily::AnthropicMessages
                        && !grapt_provider::supports_anthropic_system_updates(target.model())
                }) {
                    // The legacy protocol accepts the harness advisory as a
                    // contextual User fragment, as in the upstream boundary.
                    MessageRole::User
                } else {
                    MessageRole::System
                };
                push_context(
                    &mut rows,
                    &mut bytes,
                    boundary,
                    CodexInputItem::Message(InputMessage {
                        role,
                        content: "You are a newly spawned subagent. Your inherited parent history ends at the labelled historical parent message. User messages after that message are addressed to this child: follow your own task and later addressed input. The parent's pending spawn has already been carried out by creating this child. Do not repeat inherited spawns or waits. Complete your work and report to your parent.".to_owned(),
                    }),
                )?;
            }
        }
        let mut native_search_calls = HashSet::new();
        for message in &messages {
            if message.opened_at_seq >= ceiling {
                continue;
            }
            let Some(content) = &message.content else {
                continue;
            };
            for (index, held) in content.items().iter().enumerate() {
                if self.compatible_reasoning_item(message, held, ceiling, &mut rows, &mut bytes)? {
                    continue;
                }
                // Reasoning carries provider-specific continuation semantics;
                // do not relabel it as visible assistant text. Tool items use
                // their typed call/result projection below.
                if !matches!(
                    held.item.kind(),
                    grapt_types::ContentKind::Text | grapt_types::ContentKind::Opaque
                ) {
                    continue;
                }
                let position = held
                    .committed
                    .as_ref()
                    .ok_or_else(|| "assistant content has no durable position".to_owned())?;
                if position.seq >= ceiling {
                    continue;
                }
                let text = read_text(
                    &self.artifacts,
                    held.item.payload(),
                    held.disposition,
                    "assistant context",
                )?;
                if matches!(held.item.kind(), grapt_types::ContentKind::Opaque) {
                    let selection = message
                        .selection
                        .as_ref()
                        .ok_or("provider replay has no durable generating selection")?;
                    let replay = grapt_provider::ProviderReplay::restore(
                        selection.provider().clone(),
                        selection.model().clone(),
                        selection.variant().cloned(),
                        text.as_str(),
                    )
                    .map_err(|error| error.to_string())?;
                    // Validate even omitted artifacts above; corruption never
                    // becomes a silent pass. Leave text and tool exchanges intact.
                    if !replay.can_replay_after_prefix_rewrite(
                        grapt_types::EventSeq::new(position.seq),
                        inherited_before
                            .and_then(|first| first.checked_sub(1))
                            .map(grapt_types::EventSeq::new)
                            .max(retained.as_ref().map(|context| context.rewritten_at)),
                    ) {
                        continue;
                    }
                    if let Some(call) = replay.native_search_call() {
                        if !calls.iter().any(|held| {
                            held.call == *call
                                && held.execution == grapt_types::ToolExecution::ProviderHosted
                                && held
                                    .tool
                                    .as_ref()
                                    .is_some_and(|tool| tool.as_str() == "web_search")
                        }) || !native_search_calls.insert(call.clone())
                        {
                            return Err("native search replay has no unique hosted call".into());
                        }
                    }
                    if replay.is_responses_reasoning() {
                        if summarized.is_some_and(|head| position.seq <= head) {
                            continue;
                        }
                        match reasoning_bytes.checked_add(replay.byte_len()) {
                            Some(next) if next <= MAX_REASONING_REPLAY_BYTES => {
                                reasoning_bytes = next;
                            }
                            _ => continue,
                        }
                    }
                    push_context(
                        &mut rows,
                        &mut bytes,
                        position.seq,
                        CodexInputItem::ProviderReplay(replay),
                    )?;
                    continue;
                }
                // append_finished captures text and native replay atomically
                // as consecutive events. Only the immediately following item
                // can replace this exact rendered text; equal text elsewhere
                // in the message is independent conversation content.
                if let Some(next) = content.items().get(index + 1).filter(|next| {
                    matches!(next.item.kind(), grapt_types::ContentKind::Opaque)
                        && held.item.ordinal().next() == Some(next.item.ordinal())
                        && next
                            .committed
                            .as_ref()
                            .is_some_and(|next| position.seq.checked_add(1) == Some(next.seq))
                }) {
                    let selection = message
                        .selection
                        .as_ref()
                        .ok_or("provider replay has no durable generating selection")?;
                    let native = read_text(
                        &self.artifacts,
                        next.item.payload(),
                        next.disposition,
                        "assistant native text",
                    )?;
                    let replay = grapt_provider::ProviderReplay::restore(
                        selection.provider().clone(),
                        selection.model().clone(),
                        selection.variant().cloned(),
                        native.as_str(),
                    )
                    .map_err(|error| error.to_string())?;
                    if replay.anthropic_rendered_text() == Some(text.as_str())
                        || replay.gemini_rendered_text() == Some(text.as_str())
                    {
                        continue;
                    }
                }
                push_context(
                    &mut rows,
                    &mut bytes,
                    position.seq,
                    CodexInputItem::Message(InputMessage {
                        role: MessageRole::Assistant,
                        content: text.as_str().to_owned(),
                    }),
                )?;
            }
        }
        // A steer admitted into an execution in flight becomes the current
        // input after the turn's exchange was already issued. The exchange a
        // continued Step must carry can therefore originate from the
        // execution's earlier input; a queue input promoted at idle never
        // reopens a finished turn, so only a steer counts those.
        let current_was_steered = inbox
            .store()
            .connection()
            .query_row(
                "SELECT delivery = 'steer' FROM session_message
                 WHERE id = ?1 AND session_id = ?2 AND type IN ('user', 'synthetic')",
                (self.input.as_str(), self.session.as_str()),
                |row| row.get(0),
            )
            .map_err(|error| format!("current input delivery is unreadable: {error}"))?;
        let mut current_exchanges = 0usize;
        // Calls of one assistant message share its originating input: one
        // indexed read per message, not per call (SC-COMPACT-01 fix 1).
        let mut inputs_by_opening: HashMap<u64, MessageId> = HashMap::new();
        let mut execution_inputs_by_opening: HashMap<u64, Option<MessageId>> = HashMap::new();
        // Each lookup keeps its first match, as the linear scans it replaces.
        let mut permissions_by_call = HashMap::new();
        for entry in &permissions {
            permissions_by_call.entry(&entry.call).or_insert(entry);
        }
        let mut messages_by_id = HashMap::new();
        for message in &messages {
            messages_by_id.entry(&message.message).or_insert(message);
        }
        let mut outputs_by_call: HashMap<_, Vec<_>> = HashMap::new();
        for entry in &outputs {
            outputs_by_call.entry(&entry.call).or_default().push(entry);
        }
        for call in &calls {
            if call.seq >= ceiling {
                continue;
            }
            let permission = permissions_by_call.get(&call.call).copied();
            let message = *messages_by_id
                .get(&call.message)
                .ok_or("tool call has no retained assistant message")?;
            // Association survives transcript compaction. Read only the durable
            // input identity; older prompt text must stay in the summary.
            let input = match inputs_by_opening.get(&message.opened_at_seq) {
                Some(input) => input.clone(),
                None => {
                    let input = inbox
                        .promoted_input_before(
                            &self.session,
                            grapt_types::EventSeq::new(message.opened_at_seq),
                        )
                        .map_err(|error| format!("tool input association is unreadable: {error}"))?
                        .ok_or("tool call has no preceding promoted input")?;
                    inputs_by_opening.insert(message.opened_at_seq, input.clone());
                    input
                }
            };
            if let Some(permission) = permission {
                // A busy Steer advances the latest input while the running
                // execution and its tool runner retain their original input.
                let original = match execution_inputs_by_opening.get(&message.opened_at_seq) {
                    Some(original) => original.clone(),
                    None => {
                        let original = queries::execution_input_before(
                            &inbox,
                            &self.session,
                            message.opened_at_seq,
                        )?;
                        execution_inputs_by_opening.insert(message.opened_at_seq, original.clone());
                        original
                    }
                };
                // Historical rows without an execution association retain their
                // promoted-input association.
                if original.as_ref().unwrap_or(&input) != &permission.input {
                    return Err("tool permission names a different originating input".into());
                }
            }
            let mut matching_outputs = outputs_by_call
                .get(&call.call)
                .into_iter()
                .flatten()
                .copied()
                .filter(|entry| entry.seq < ceiling);
            let Some(output) = matching_outputs.next() else {
                // A new user input may inspect an old ended Step. A failed
                // Step can lose an outcome; an ended one can only have lost it
                // to an unreadable terminal row, which proves nothing. Either is
                // historical uncertainty, never a successful result or
                // authority to resume/replay that tool. Live missing outcomes
                // still refuse below.
                let ended_before_input = messages.iter().find(|message| {
                    message.message == call.message
                        && message.terminal_committed.as_ref().is_some_and(|position| {
                            position.seq > call.seq && position.seq < users[current].seq
                        })
                });
                let Some(ended) = ended_before_input.filter(|_| input != self.input) else {
                    return Err(format!("tool {} has no durable terminal output", call.call).into());
                };
                let from = match ended.terminal {
                    Some(grapt_session::StepTerminal::Failed { .. }) => " from a failed Step",
                    _ => "",
                };
                push_context(
                    &mut rows,
                    &mut bytes,
                    call.seq,
                    CodexInputItem::Message(InputMessage {
                        role: MessageRole::Assistant,
                        content: format!(
                            "[Historical tool {}{from} has no readable recorded outcome. Its effects are unknown. Inspect current state before deciding on new work; this is not an instruction to replay it.]",
                            call.call
                        ),
                    }),
                )?;
                continue;
            };
            if matching_outputs.next().is_some() {
                return Err(
                    format!("tool {} has duplicate durable terminal outputs", call.call).into(),
                );
            }
            if output.seq <= call.seq {
                return Err("tool output precedes its call".into());
            }
            let Some(tool) = call.tool.clone() else {
                // Old failed-before-permission calls did not preserve a name.
                // Keep the failure visible without inventing a callable identity.
                push_context(&mut rows, &mut bytes, call.seq,
                    CodexInputItem::Message(InputMessage { role: MessageRole::Assistant,
                        content: format!("[Historical tool {} has no recorded name; its recorded outcome follows: {}]", call.call, output.output.as_str()),
                    }))?;
                continue;
            };
            if input == self.input || current_was_steered {
                current_exchanges += 1;
            }
            if call.execution == grapt_types::ToolExecution::ProviderHosted {
                if native_search_calls.contains(&call.call) {
                    continue;
                }
                // A hosted result is recorded data, never a function exchange
                // that this harness may execute. Native adapter replay is
                // captured separately as opaque provider-owned content.
                push_context(&mut rows, &mut bytes, call.seq,
                    CodexInputItem::Message(InputMessage {
                        role: MessageRole::Assistant,
                        content: format!("[Provider-hosted tool {}. Outcome: {}.\nArguments: {}\nRecorded result: {}]",
                            tool.as_str(), if output.error.is_some() { "failed" } else { "completed" },
                            call.arguments.as_str(), output.output.as_str()),
                    }))?;
                continue;
            }
            let receipt = (tool.as_str() == tools::image::TOOL
                && permission.map_or_else(
                    || {
                        call.image_read.is_some()
                            || tools::image::has_receipt(output.output.as_str())
                    },
                    |held| held.tool.as_str() == "read",
                )
                && output.error.is_none())
            .then(|| tools::image::replay(output.output.as_str()));
            let provider_output = match receipt {
                Some(Ok((description, images))) => CodexInputItem::ImageToolOutput {
                    call: output.call.clone(),
                    output: description,
                    images,
                },
                // A receipt that does not parse is a lost output, as one
                // failing its digest is.
                Some(Err(error)) => {
                    crate::diagnostics::report(format!(
                        "native image receipt of {} is corrupt: {error}",
                        output.call
                    ));
                    CodexInputItem::ToolOutput {
                        call: output.call.clone(),
                        output: CodexToolText::try_new(super::artifact_reads::unreadable_output(
                            "image output",
                        ))
                        .map_err(|error| error.to_string())?,
                        is_error: true,
                    }
                }
                None => CodexInputItem::ToolOutput {
                    call: output.call.clone(),
                    output: shell_preview::project(tool.as_str(), &output.call, &output.output)?,
                    is_error: output.error.is_some(),
                },
            };
            push_context(
                &mut rows,
                &mut bytes,
                call.seq,
                tools::context_call(
                    call.call.clone(),
                    tool,
                    call.arguments.clone(),
                    permission.map(|held| held.tool.as_str()),
                )?,
            )?;
            push_context(&mut rows, &mut bytes, output.seq, provider_output)?;
        }
        if continuation_required && current_exchanges == 0 && retained.is_none() {
            return Err("continued Step has no complete durable tool exchange".into());
        }
        for (seq, item) in mail::rows(inbox.store(), &self.session, from, until, ceiling)? {
            push_context(&mut rows, &mut bytes, seq, item)?;
        }
        for (seq, content) in grapt_inbox::operation_mail::presented_rows_classified(
            inbox.store(),
            &self.session,
            from,
            until,
            ceiling,
            None,
        )? {
            push_context(
                &mut rows,
                &mut bytes,
                seq,
                CodexInputItem::Message(InputMessage {
                    role: MessageRole::User,
                    content,
                }),
            )?;
        }
        for (seq, item) in output_recovery::rows(&inbox, &self.session, &messages, from, ceiling)? {
            push_context(&mut rows, &mut bytes, seq, item)?;
        }
        rows.sort_by_key(|(seq, _)| *seq);
        if rows.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err("two conversation items claim the same durable sequence".into());
        }
        let efforts = effort::updates(users, &messages, ceiling, lock(&self.redeemer).as_ref());
        bytes = bytes.saturating_add(
            efforts
                .values()
                .map(|value| value.as_str().len())
                .sum::<usize>(),
        );
        if bytes > MAX_CONTEXT_BYTES {
            return Err(ContextError::over_bytes(Bound::Conversation, bytes));
        }
        let response_order = response_order::ResponseOrder::prepare(
            &rows,
            &messages,
            &response_order::Calls(&calls),
        )?;
        let grouped = rows.len().saturating_add(response_order.boundaries());
        if grouped > MAX_CONTEXT_ITEMS {
            return Err(ContextError::over_items(Bound::ResponseGroups, grouped));
        }
        if grouped.saturating_add(efforts.len()) > MAX_CONTEXT_ITEMS {
            return Err(ContextError::over_items(
                Bound::Conversation,
                grouped.saturating_add(efforts.len()),
            ));
        }
        let identity = retained.as_ref().map(|retained| retained.identity.clone());
        let mut prefix = match &retained {
            Some(retained) => retained_users::prefix(
                &inbox,
                &self.artifacts,
                &self.session,
                &retained.input,
                retained.from,
                retained.original_users,
            )?,
            None => Vec::new(),
        };
        if let Some(retained) = &retained {
            prefix.push(recovery_reference::item(
                &inbox,
                &self.session,
                &retained.compaction,
                retained.from,
                retained.source_head,
            )?);
        }
        let originals = retained
            .as_ref()
            .is_some_and(|retained| retained.original_users);
        let (summary, provider) = match retained.map(|retained| retained.summary) {
            Some(retained::RetainedSummary::Text(text)) => (Some(text), Vec::new()),
            Some(retained::RetainedSummary::Provider(items)) => (
                None,
                items
                    .into_iter()
                    .map(CodexInputItem::ProviderReplay)
                    .collect(),
            ),
            None => (None, Vec::new()),
        };
        if summary.is_some() || !provider.is_empty() {
            context_error::retained_fit(
                bytes,
                grouped.saturating_add(efforts.len()),
                &prefix,
                summary.as_deref(),
                &provider,
                originals,
            )?;
        }
        Ok(RequestContext {
            current_input,
            ceiling,
            rows,
            response_order,
            efforts,
            summary,
            original_users: originals,
            provider,
            identity,
            prefix,
        })
    }
    /// Keep the compaction's original input and the current input exact in
    /// their durable positions, even when the retained cut precedes them.
    fn include_current_inputs(
        &self,
        inbox: &Inbox,
        users: &mut Vec<grapt_inbox::PromotedMessage>,
        retained_input: Option<&MessageId>,
    ) -> Result<(), ContextError> {
        for input in retained_input
            .into_iter()
            .chain(std::iter::once(&self.input))
        {
            if !users.iter().any(|user| &user.input == input) {
                let current = inbox
                    .promoted_input(&self.session, input)
                    .map_err(|error| error.to_string())?;
                users.push(grapt_inbox::PromotedMessage {
                    input: input.clone(),
                    kind: grapt_inbox::PromotedInputKind::User,
                    text: current.text,
                    description: None,
                    metadata: None,
                    seq: current.seq,
                });
            }
        }
        Ok(())
    }
}

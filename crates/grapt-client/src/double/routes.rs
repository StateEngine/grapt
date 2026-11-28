//! The double's ROUTE TABLE: one arm per operation the client can call.
//!
//! Separated from the producer's state so each file answers one question. This
//! one answers "what does the product do when asked", and `double.rs` answers
//! "what has the product been made to hold". They were one file until it passed
//! this workspace's module ceiling, and the seam was already there.

use super::*;

impl ContractDouble {
    /// One request, answered as the product would answer it.
    ///
    /// Inherent rather than the trait method itself: the `Transport` impl stays
    /// in `double.rs` beside the producer's other seams and delegates here, so
    /// moving the table did not move the seam.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn route(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        let path = request.path().to_owned();
        match request.operation() {
            Operation::IntegrationList
            | Operation::IntegrationKeyConnect
            | Operation::IntegrationOAuthConnect
            | Operation::IntegrationOAuthStatus
            | Operation::IntegrationOAuthCancel
            | Operation::IntegrationOAuthComplete
            | Operation::CredentialRename
            | Operation::CredentialRemove => Ok(WireResponse {
                status: 501,
                body: Vec::new(),
            }),
            Operation::InstructionInspect => match Self::session_of(&path) {
                Some(id) if self.sessions.contains_key(&id) => {
                    Self::ok(serde_json::json!({"captured":false,"ambient":"","keys":[]}))
                }
                _ => Ok(WireResponse {
                    status: 404,
                    body: Vec::new(),
                }),
            },
            Operation::SessionFork => Ok(WireResponse {
                status: 501,
                body: b"the contract double has no durable fork source".to_vec(),
            }),
            Operation::WorkspaceWorktree => Ok(WireResponse {
                status: 501,
                body: b"the contract double has no checkout filesystem".to_vec(),
            }),
            Operation::SessionSandboxWorktreeConfigGet
            | Operation::SessionSandboxWorktreeConfigCreate => Ok(WireResponse {
                status: 501,
                body: b"the contract double has no sandbox filesystem".to_vec(),
            }),
            Operation::SandboxGrantList | Operation::SandboxGrantRevoke => Ok(WireResponse {
                status: 501,
                body: b"the contract double has no contained commands or sandbox grants".to_vec(),
            }),
            Operation::SessionUndoPreview | Operation::SessionUndo => Ok(WireResponse {
                status: 409,
                body: b"the contract double has no patch filesystem".to_vec(),
            }),
            Operation::SessionRewindPreview | Operation::SessionRewind => {
                self.rewind_route(request)
            }
            // The double never loses a process, so nothing is ever orphaned.
            Operation::SessionReconcile => match Self::session_of(&path) {
                Some(id) if self.sessions.contains_key(&id) => Ok(WireResponse {
                    status: 200,
                    body: serde_json::json!({"data":{"sessionID":id.spelled(),"reconciled":0}})
                        .to_string()
                        .into_bytes(),
                }),
                _ => Ok(WireResponse {
                    status: 404,
                    body: Vec::new(),
                }),
            },
            Operation::SessionPresetGet | Operation::SessionPresetSelect => {
                let session = Self::session_of(&path).and_then(|id| self.sessions.get_mut(&id));
                let Some(session) = session else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                if request.operation() == Operation::SessionPresetSelect {
                    session.preset =
                        crate::wire::decode_preset_selection(request.body().unwrap_or_default())
                            .ok();
                    if session.preset.is_none() {
                        return Ok(WireResponse {
                            status: 400,
                            body: Vec::new(),
                        });
                    }
                    return Ok(WireResponse {
                        status: 204,
                        body: Vec::new(),
                    });
                }
                Self::ok(
                    serde_json::json!({"data":{"selected":session.preset.map(crate::preset::name),"effective":crate::preset::name(session.preset.unwrap_or(crate::preset::Preset::Author))}}),
                )
            }
            Operation::CommandList | Operation::CommandInspect => {
                let Some(project) = self.located(request) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                Self::ok(
                    serde_json::json!({"location": serde_json::json!({"directory":project.worktree,"project":{"id":project.id.spelled(),"directory":project.worktree}}), "data": []}),
                )
            }
            Operation::HealthGet => Self::ok(serde_json::json!({"status": "ok"})),
            Operation::WorkspaceDiff
            | Operation::ProjectList
            | Operation::ProjectCurrent
            | Operation::AgentList
            | Operation::ModelList
            | Operation::ModelDefault => self.catalog_route(request),
            Operation::ProviderList => {
                let Some(project) = self.located(request) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                Self::ok(
                    serde_json::json!({"location": serde_json::json!({"directory":project.worktree,"project":{"id":project.id.spelled(),"directory":project.worktree}}), "data": [
                        {"id":"double", "name":"Double", "package":"builtin:double", "settings":{"account":"account-primary"}},
                        {"id":"other", "name":"Other", "package":"builtin:double", "settings":{"account":"account-secondary"}}
                    ]}),
                )
            }
            Operation::McpList | Operation::McpInspect if self.fault == Fault::McpUnavailable => {
                Ok(WireResponse {
                    status: 400,
                    body: Vec::new(),
                })
            }
            Operation::McpList | Operation::McpInspect => {
                // Every list request observed, the ones that fail location
                // resolution below included: the witness arms the bare 400
                // from the read past a count.
                self.mcp_list_reads += 1;
                let Some(project) = self.located(request) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                // The unreadable config: the bare 400 with no body, named for
                // this project's worktree, or armed for the reads past a
                // count on any project.
                if self.mcp_unavailable.contains(&project.worktree)
                    || (self.mcp_fail_after_reads > 0
                        && self.mcp_list_reads > self.mcp_fail_after_reads)
                {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                }
                // The refused-config shape: the 400 body is the frozen
                // InvalidRequestError, and its message — which the product
                // spells with the faulting file's name — is the whole
                // answer. A 500 here would mean this double could not spell
                // the one body it was asked for.
                if let Some(message) = &self.mcp_refusal {
                    return match crate::wire::encode_invalid_request(message) {
                        Ok(body) => Ok(WireResponse { status: 400, body }),
                        Err(_) => Ok(WireResponse {
                            status: 500,
                            body: Vec::new(),
                        }),
                    };
                }
                let encoded = if request.operation() == Operation::McpInspect {
                    let mut rows = self.mcp_servers.clone();
                    for row in &mut rows {
                        row.tools = Some(
                            if matches!(row.status, crate::wire::McpServerStatus::Connected) {
                                vec![
                                    crate::wire::McpToolRow {
                                        name: format!("{}_query", row.name),
                                        omission: None,
                                    },
                                    crate::wire::McpToolRow {
                                        name: format!("{}_withheld", row.name),
                                        omission: Some("built-in name collision".to_owned()),
                                    },
                                ]
                            } else {
                                Vec::new()
                            },
                        );
                    }
                    crate::wire::encode_mcp_inspection(project, &rows)
                } else {
                    crate::wire::encode_mcp_servers(project, &self.mcp_servers)
                };
                match encoded {
                    Ok(body) => Ok(WireResponse { status: 200, body }),
                    Err(_) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                }
            }
            Operation::SessionDelete => self.delete_session(request),
            Operation::SessionRename => self.rename_session(request),
            Operation::SessionAgentSelect => {
                let Some(session) = Self::session_of(&path) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                let Some(bytes) = request.body() else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                let Ok(agent) = crate::wire::decode_agent_select(bytes) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                // An agent outside the served roster is a 400 rather than a
                // silent accept. A double that accepted anything would let the
                // client ship a selection the product refuses.
                if !self.agents.iter().any(|row| row.id == agent) {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                }
                let Some(entry) = self.sessions.get_mut(&session) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                entry.agent = Some(agent);
                // 204, exactly as the frozen route declares. A 200 with a body
                // would be a shape the real product never produces.
                Ok(WireResponse {
                    status: 204,
                    body: Vec::new(),
                })
            }
            Operation::SessionModelSelect => {
                let Some(session) = Self::session_of(&path) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                let Some(bytes) = request.body() else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                let Ok((model, provider)) = crate::wire::decode_model_select(bytes) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                // Matched on the PAIR. A model id alone is not unique across
                // providers, so accepting it alone here would make the double
                // agree with a client defect the product would not.
                let known = self
                    .models
                    .iter()
                    .any(|row| row.id == model && row.provider_id == provider && row.enabled);
                if !known {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                }
                let Some(entry) = self.sessions.get_mut(&session) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                entry.variant = crate::wire::decode_model_variant(bytes)
                    .map_err(|_| TransportError::NotConnected)?;
                entry.model = Some((model, provider));
                Ok(WireResponse {
                    status: 204,
                    body: Vec::new(),
                })
            }
            Operation::GlobalFormGet
            | Operation::GlobalFormState
            | Operation::GlobalFormReply
            | Operation::GlobalFormCancel => Ok(WireResponse {
                status: 404,
                body: Vec::new(),
            }),
            Operation::FormRequestList => {
                let Some(project) = self.located(request) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                // Only OPEN forms are on the outstanding surface. Suspended
                // counts as open: a shutdown does not answer a question, and a
                // client that stopped listing it would lose the only route back
                // to a form the product still expects an answer for.
                let rows: Vec<crate::wire::FormRow> = self
                    .forms
                    .iter()
                    .filter(|form| form.state.is_open())
                    .map(|form| form.row.clone())
                    .collect();
                match crate::wire::encode_form_requests(project, &rows) {
                    Ok(body) => Ok(WireResponse { status: 200, body }),
                    Err(_) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                }
            }
            Operation::SessionFormList if self.fault == Fault::FormsUnavailable => {
                Ok(WireResponse {
                    status: 503,
                    body: Vec::new(),
                })
            }
            Operation::SessionFormList => {
                // A QUESTION THAT ARRIVES WHILE THE CLIENT IS LOOKING. Like the
                // permission reveal above, the producer raises it after a
                // declared number of reads rather than before the first one —
                // so a form scheduled this way can be seen ONLY by a client
                // that reads the list again on its own. A form raised up front
                // is delivered by the read every client makes at startup, and a
                // witness built on one proves nothing about polling.
                if let Some((session, title, after)) = self.scheduled_form.clone() {
                    self.form_list_reads += 1;
                    if self.form_list_reads >= after {
                        self.scheduled_form = None;
                        let _ = self.raise_form(&session, &title);
                    }
                }
                let Some(session) = Self::session_of(&path) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                if !self.sessions.contains_key(&session) {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                }
                // Scoped by the session in the PATH, so a client that read the
                // per-session route and rendered every form on the floor would
                // fail here rather than showing one session's questions under
                // another session's transcript.
                let rows: Vec<crate::wire::FormRow> = self
                    .forms
                    .iter()
                    .filter(|form| {
                        form.row.owner.session() == Some(&session) && form.state.is_open()
                    })
                    .map(|form| form.row.clone())
                    .collect();
                match crate::wire::encode_session_forms(&rows) {
                    Ok(body) => Ok(WireResponse { status: 200, body }),
                    Err(_) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                }
            }
            Operation::SessionList => match self.session_page() {
                Ok(body) => Ok(WireResponse { status: 200, body }),
                Err(_) => Ok(WireResponse {
                    status: 500,
                    body: Vec::new(),
                }),
            },
            Operation::SessionCreate => {
                // The selection is READ from the request and recorded, so a
                // client that held a selection locally without sending it
                // creates a session this double reports as unselected. A double
                // that ignored the body would let that client pass the journey
                // witness while the real product defaulted the session.
                let Some(bytes) = request.body() else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                let Ok(selection) = crate::wire::decode_session_create(bytes) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                let directory = selection.directory.unwrap_or_default();
                // The location must name a project this double serves. An
                // unknown directory is a 400: creating a session at a location
                // the catalog never offered is how a client ends up chatting
                // outside the project the screen names.
                if !self
                    .projects
                    .iter()
                    .any(|project| project.worktree == directory)
                {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                }
                // The session-retention bound refuses VISIBLY, over the wire, as
                // a 503. A producer at its bound is declining work it is bounded
                // from doing — the same disposition the host's own admission
                // bound takes — and it is emphatically not a 500, which would
                // report a bound working correctly as a fault.
                let Some(id) = self.create_session("session") else {
                    return Ok(WireResponse {
                        status: 503,
                        body: Vec::new(),
                    });
                };
                let project = self
                    .projects
                    .iter()
                    .find(|row| row.worktree == directory)
                    .cloned();
                let (title, parent) = match self.sessions.get_mut(&id) {
                    Some(entry) => {
                        entry.project = project;
                        entry.agent = selection.agent;
                        entry.model = selection.model;
                        entry.variant = selection.variant;
                        (entry.title.clone(), entry.parent.clone())
                    }
                    None => (String::new(), None),
                };
                match crate::wire::session_info(&id, &title, &directory, parent.as_ref())
                    .and_then(crate::wire::encode_session)
                {
                    Ok(body) => Ok(WireResponse { status: 200, body }),
                    Err(_) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                }
            }
            Operation::SessionGet => {
                let Some(session) = Self::session_of(&path) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                match self.session_snapshot(&session) {
                    Some(Ok(body)) => Ok(WireResponse { status: 200, body }),
                    Some(Err(_)) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                    None => Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    }),
                }
            }
            Operation::MessageList if self.fault == Fault::MessageListUnavailable => {
                self.fault = Fault::None;
                Ok(WireResponse {
                    status: 503,
                    body: Vec::new(),
                })
            }
            Operation::MessageList => {
                self.message_list_calls = self.message_list_calls.saturating_add(1);
                let Some(session) = Self::session_of(&path) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                match self.message_page(&session, request) {
                    Ok(body) => Ok(WireResponse { status: 200, body }),
                    Err(_) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                }
            }
            Operation::SessionPendingList => {
                if Self::session_of(&path).is_some_and(|id| self.sessions.contains_key(&id)) {
                    Self::ok(serde_json::json!({"data": []}))
                } else {
                    Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    })
                }
            }
            Operation::SessionPrompt => {
                let Some(session) = Self::session_of(&path) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                // Decoded through the REAL request DTO. A body the contract
                // refuses is a 400 here, not a silently defaulted empty string.
                let Some(bytes) = request.body() else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                let Ok(text) = crate::wire::decode_prompt(bytes) else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                // The double's scripted vocabulary. A prompt beginning `ask:`
                // raises a permission and one beginning `form:` raises a form,
                // instead of running a turn.
                //
                // This exists so the SHIPPED BINARY can reach the two states a
                // run waits in. Every in-crate witness raises them directly
                // through `ask_permission` and `raise_form`, which a separate
                // process cannot call — so without a prompt that means "wait
                // for me", the permission and form panes would be provable only
                // in memory and never capturable from the real client under a
                // real terminal. That is exactly the gap the PTY harness exists
                // to close, and it is where three shipped defects were already
                // found.
                //
                // It is a property of the DOUBLE and never of the product: the
                // real product raises these when its own work needs an answer.
                if let Some(title) = text.strip_prefix("ask:") {
                    let _ = self.ask_permission(&session, title.trim());
                    return Self::ok(serde_json::json!({"accepted": true}));
                }
                if let Some(title) = text.strip_prefix("form:") {
                    let _ = self.raise_form(&session, title.trim());
                    return Self::ok(serde_json::json!({"accepted": true}));
                }
                // An input id already admitted, as the product's inbox reads
                // it: an exact retry in the same session is answered, not run
                // again; the same id with another session or text is a 409.
                let input = crate::wire::decode_prompt_id(bytes).ok().flatten();
                if let Some(held) = input.as_deref().and_then(|id| self.admitted_input(id)) {
                    if held == (&session, text.as_str()) {
                        return Self::ok(serde_json::json!({"accepted": true}));
                    }
                    return Ok(WireResponse {
                        status: 409,
                        body: Vec::new(),
                    });
                }
                let lost = || TransportError::AfterSend {
                    reason: "the double lost the reply".to_owned(),
                };
                if self.fault == Fault::PromptLostBeforeAdmission {
                    self.fault = Fault::None;
                    return Err(lost());
                }
                self.run_turn_as(&session, &text, &format!("echo {text}"), input);
                match self.fault {
                    Fault::PromptReplyLost => {
                        self.fault = Fault::None;
                        Err(lost())
                    }
                    Fault::PromptReplyLostUnconfirmed => {
                        self.fault = Fault::MessageListUnavailable;
                        Err(lost())
                    }
                    _ => Self::ok(serde_json::json!({"accepted": true})),
                }
            }
            Operation::SessionInterrupt => {
                let Some(session) = Self::session_of(&path) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                let execution = self.mint("exe");
                let data = serde_json::json!({
                    "sessionID": session.spelled(),
                    "executionID": execution
                });
                // The real terminal is committed before publication. Hydration
                // must see the interruption, not an older successful message.
                self.record(&session, DoubleMessage::Interrupted);
                self.append(&session, EventMember::ExecutionStarted, data.clone());
                self.append(&session, EventMember::ExecutionInterrupted, data);
                self.observe_state(&session, "interrupted", Some(execution));
                // As the product does: interrupting cancels the tool that was
                // asking, so its ask leaves the open-request list.
                self.withdraw_permissions(&session);
                Self::ok(serde_json::json!({"interrupted": true}))
            }
            Operation::SessionCompact => {
                // As the product: only a known session is admitted.
                let Some(session) =
                    Self::session_of(&path).filter(|session| self.sessions.contains_key(session))
                else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                if crate::wire::decode_compact(request.body().unwrap_or_default()).is_err() {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                }
                if self.compactions.len() >= self.bounds.max_retained_log_entries {
                    return Ok(WireResponse {
                        status: 409,
                        body: Vec::new(),
                    });
                }
                // Admission only, as the product: the compaction runs in the
                // session's next input, so nothing is appended to the feed.
                let input = self.mint("msg_");
                self.compactions.push(session.clone());
                Self::ok(serde_json::json!({"data": {
                    "admittedSeq": self.compactions.len(),
                    "id": input,
                    "sessionID": session.spelled(),
                    "timeCreated": 0,
                    "type": "compaction",
                }}))
            }
            Operation::SessionSubagentModelGet if self.fault == Fault::SubagentModelUnavailable => {
                Ok(WireResponse {
                    status: 503,
                    body: Vec::new(),
                })
            }
            Operation::SessionSubagentModelGet => {
                // NULL is a REAL value here: it means subagents inherit the
                // agent's model rather than "not answered". The double serves
                // whatever was set, defaulting to that inheritance.
                let selected = Self::session_of(&path)
                    .and_then(|session| self.subagent_models.get(&session).cloned())
                    .flatten();
                match selected {
                    Some(model) => Self::ok(serde_json::json!({
                        "data": {"id": model.0, "providerID": model.1}
                    })),
                    None => Self::ok(serde_json::json!({"data": null})),
                }
            }
            Operation::PermissionRequestList
                if self.fault == Fault::PermissionListFailsAfterAnswer
                    && self
                        .permissions
                        .values()
                        .flatten()
                        .any(|ask| ask.settled.is_some()) =>
            {
                Ok(WireResponse {
                    status: 503,
                    body: Vec::new(),
                })
            }
            Operation::PermissionRequestList => {
                self.permission_list_calls += 1;
                if let Some(session) = self.blocked_session.clone() {
                    self.reads_since_blocked += 1;
                    // The tool had not asked yet at the startup read, nor at the
                    // one the client makes the instant a prompt is submitted. It
                    // asks now, which only a MID-RUN read can observe.
                    if self.reads_since_blocked >= self.reveal_after_reads {
                        self.blocked_session = None;
                        let _ = self.ask_permission(&session, "shell: write j03.txt");
                    }
                }
                let rows: Vec<serde_json::Value> = self
                    .permissions
                    .iter()
                    .flat_map(|(session, asks)| {
                        asks.iter()
                            .filter(|ask| ask.settled.is_none())
                            .map(move |ask| ask.row(session))
                    })
                    .collect();
                Self::ok(serde_json::json!({
                    "location": {
                        "directory": "/work",
                        "project": {"id": "prj_double", "directory": "/work"}
                    },
                    "data": rows,
                    "hasMore": false
                }))
            }
            Operation::SessionPermissionList => {
                let Some(session) = Self::session_of(&path) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                let rows: Vec<serde_json::Value> = self
                    .permissions(&session)
                    .iter()
                    .filter(|ask| ask.settled.is_none())
                    .map(|ask| ask.row(&session))
                    .collect();
                Self::ok(serde_json::json!({"data": rows, "hasMore": false}))
            }
            Operation::SessionPermissionCreate => {
                let Some(session) = Self::session_of(&path) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                let Some(body) = request.body() else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                let action = match crate::wire::decode_permission_create(body) {
                    Ok(action) => action,
                    Err(_) => {
                        return Ok(WireResponse {
                            status: 400,
                            body: Vec::new(),
                        })
                    }
                };
                match self.ask_permission(&session, &action) {
                    Some(id) => Self::ok(serde_json::json!({"data": {"id": id, "effect": "ask"}})),
                    None => Ok(WireResponse {
                        status: 503,
                        body: Vec::new(),
                    }),
                }
            }
            Operation::SessionPermissionGet => {
                let (Some(session), Some(request_id)) =
                    (Self::session_of(&path), Self::request_of(&path))
                else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                match self
                    .permissions(&session)
                    .iter()
                    .find(|ask| ask.request_id == request_id)
                {
                    Some(ask) if ask.settled.is_none() => Self::ok(serde_json::json!({"data": {
                        "id": ask.request_id,
                        "sessionID": session.spelled(),
                        "action": ask.title,
                        "resources": [],
                        "status": "pending"
                    }})),
                    Some(_) | None => Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    }),
                }
            }
            Operation::SessionPermissionReply if self.fault == Fault::PermissionReplyConflict => {
                Ok(WireResponse {
                    status: 409,
                    body: Vec::new(),
                })
            }
            Operation::SessionPermissionReply => {
                let (Some(session), Some(request_id)) = (
                    Self::session_of(&path),
                    Self::request_of(&path).map(str::to_owned),
                ) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                let Some(body) = request.body() else {
                    return Ok(WireResponse {
                        status: 400,
                        body: Vec::new(),
                    });
                };
                let choice = match crate::wire::decode_permission_reply(body) {
                    Ok(choice) => choice,
                    Err(_) => {
                        return Ok(WireResponse {
                            status: 400,
                            body: Vec::new(),
                        })
                    }
                };
                let Some(asks) = self.permissions.get_mut(&session) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                let Some(ask) = asks.iter_mut().find(|ask| ask.request_id == request_id) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                if ask.settled.is_some() {
                    // The open-request surface no longer exposes terminal
                    // rows. A conflicting second answer therefore has the
                    // same permission-not-found shape as the real product.
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                }
                ask.settled = Some(choice);
                Ok(WireResponse {
                    status: 204,
                    body: Vec::new(),
                })
            }
            Operation::SessionFormGet | Operation::SessionFormState => {
                let (Some(session), Some(form_id)) = (
                    Self::session_of(&path),
                    Self::form_of(&path).map(str::to_owned),
                ) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                let Some(form) = self.forms.iter().find(|form| {
                    form.row.id.spelled() == form_id && form.row.owner.session() == Some(&session)
                }) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                // A settled form is still READABLE by its own identity. Only
                // the outstanding LIST drops it: "no longer outstanding" and
                // "no longer exists" are different answers, and a client that
                // could not tell them apart could not report an answered form
                // as answered.
                let encoded = if request.operation() == Operation::SessionFormGet {
                    crate::wire::encode_form(&form.row)
                } else {
                    crate::wire::encode_form_state(&form.state)
                };
                match encoded {
                    Ok(body) => Ok(WireResponse { status: 200, body }),
                    Err(_) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                }
            }
            Operation::SessionFormReply | Operation::SessionFormCancel => {
                let cancelling = request.operation() == Operation::SessionFormCancel;
                let (Some(session), Some(form_id)) = (
                    Self::session_of(&path),
                    Self::form_of(&path).map(str::to_owned),
                ) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                // The answer is decoded BEFORE the form is looked up, so an
                // unparseable body is 400 whatever the form's state is. A
                // decode after the settled check would report `409` for a
                // request the server could never have applied.
                let answer = if cancelling {
                    None
                } else {
                    let Some(body) = request.body() else {
                        return Ok(WireResponse {
                            status: 400,
                            body: Vec::new(),
                        });
                    };
                    match crate::wire::decode_form_reply(body) {
                        Ok(answer) => Some(answer),
                        Err(_) => {
                            return Ok(WireResponse {
                                status: 400,
                                body: Vec::new(),
                            })
                        }
                    }
                };
                let Some(form) = self.forms.iter_mut().find(|form| {
                    form.row.id.spelled() == form_id && form.row.owner.session() == Some(&session)
                }) else {
                    return Ok(WireResponse {
                        status: 404,
                        body: Vec::new(),
                    });
                };
                if !form.state.is_open() {
                    // 409, NOT the permission surface's 404. The frozen record
                    // gives forms their own `FormAlreadySettledError`, and the
                    // difference is the whole anti-resurrection property: "you
                    // are too late" is a different fact from "there is no such
                    // form", and a client told the wrong one would re-ask.
                    return Ok(WireResponse {
                        status: 409,
                        body: Vec::new(),
                    });
                }
                // A SUSPENDED form settles here exactly as a pending one does.
                // That is the point: suspension is nonterminal, so a later
                // answer is truthful, and it settles the FORM without
                // resurrecting the waiter that was lost with the process.
                form.state = match answer {
                    Some(answer) => crate::wire::FormState::Answered(answer),
                    None => crate::wire::FormState::Cancelled,
                };
                Ok(WireResponse {
                    status: 204,
                    body: Vec::new(),
                })
            }
            Operation::McpContent
            | Operation::McpAuth
            | Operation::McpConfiguration
            | Operation::McpReconnect
            | Operation::McpApproval
            | Operation::ClaudePlugin
            | Operation::SideQuestion => Ok(WireResponse {
                status: 501,
                body: Vec::new(),
            }),
            Operation::EventSubscribe => Ok(WireResponse {
                status: 200,
                body: Vec::new(),
            }),
            // An unknown session is the product's typed not-found.
            Operation::SessionSnapshot => {
                let session = Self::session_of(&path);
                let snapshot = session
                    .as_ref()
                    .map(|session| self.coherent_snapshot(session))
                    .transpose();
                match snapshot {
                    Ok(Some(Some(body))) => Ok(WireResponse { status: 200, body }),
                    Ok(_) => Ok(WireResponse {
                        status: 404,
                        body: serde_json::json!({
                            "_tag": "SessionNotFoundError",
                            "sessionID": session.as_ref().map(|session| session.spelled()),
                            "message": "Session was not found",
                        })
                        .to_string()
                        .into_bytes(),
                    }),
                    Err(_) => Ok(WireResponse {
                        status: 500,
                        body: Vec::new(),
                    }),
                }
            }
        }
    }
}

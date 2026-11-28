use super::*;

pub(in crate::dogfood) mod auto_resume;
mod owned;

impl ServerApplication {
    #[allow(
        clippy::too_many_arguments,
        reason = "typed synthetic admission coordinates"
    )]
    pub(crate) async fn submit_synthetic(
        &self,
        principal: SealedPrincipal,
        session: SessionId,
        input: MessageId,
        text: String,
        description: Option<String>,
        metadata: Option<Value>,
        delivery: DeliveryMode,
        resume: bool,
    ) -> Result<Value, String> {
        let record = self.get_record(&session)?;
        let agent = grapt_types::AgentId::new(record.agent.unwrap_or_else(|| "default".to_owned()))
            .ok_or_else(|| "session agent identity is invalid".to_owned())?;
        self.ensure_session_writer(&session)?;
        subagent::claim_single_prompt(self, &session)?;
        let reservation = if resume {
            Some(self.prompt_mailbox.reserve()?)
        } else {
            None
        };
        let wake = if !resume {
            None
        } else {
            Some(
                self.wake_slots
                    .reserve_for_workspace(&session, Path::new(record.placement.directory()))?,
            )
        };
        let event = self.mint_event()?;
        let created_ms = self.clock.now().as_millis();
        let admission = self
            .lock_inbox()
            .admit_synthetic(grapt_inbox::SyntheticAdmitRequest {
                event_id: &event,
                session: &session,
                input: &input,
                text: &text,
                description: description.as_deref(),
                metadata: metadata.as_ref(),
                delivery,
                resume,
                created_ms,
            })
            .map_err(|error| error.to_string())?;
        let seq = match &admission {
            AdmitOutcome::Admitted { seq, .. } => *seq,
            AdmitOutcome::Reconciled { .. } => {
                self.lock_inbox()
                    .admission_coordinates(&input)
                    .map_err(|error| error.to_string())?
                    .seq
            }
        };
        if matches!(admission, AdmitOutcome::Admitted { .. }) {
            input_events::publish(&self.lock_inbox(), &self.hub, &session, seq);
            self.agent_activity.send_replace(());
        }
        let mut data = json!({"text":text});
        if let Some(description) = description {
            data["description"] = json!(description);
        }
        if let Some(metadata) = metadata {
            data["metadata"] = metadata;
        }
        let response = json!({
            "admittedSeq":seq,
            "id":input.as_str(),
            "sessionID":session.as_str(),
            "timeCreated":created_ms as f64,
            "type":"synthetic",
            "data":data,
            "delivery":match delivery { DeliveryMode::Steer => "steer", DeliveryMode::Queue => "queue" },
        });
        if !should_execute(&admission) {
            return Ok(response);
        }
        let reservation = reservation
            .ok_or_else(|| "synthetic wake lost its prompt execution reservation".to_owned())?;
        let accepted = self.schedule_admitted(
            principal,
            session,
            input,
            seq,
            agent,
            reservation,
            wake,
            Some(self.mint_execution()?),
            None,
        )?;
        accepted
            .await
            .map_err(|_| "execution owner closed before accepting custody".to_owned())??;
        Ok(response)
    }

    pub(crate) fn submit_prompt(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: PromptRequestDto,
    ) -> ApplicationFuture<PromptResponseDto, SessionPromptApplicationError> {
        self.submit_prompt_run(principal, session_id, request, None)
    }

    /// The operation's initial tool prompt owns this exact execution. Human
    /// followups use normal Session admission without settling that operation.
    pub(crate) fn submit_child_prompt(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: PromptRequestDto,
        execution: ExecutionId,
    ) -> ApplicationFuture<PromptResponseDto, SessionPromptApplicationError> {
        let application = self.clone();
        Box::pin(async move {
            let session = session_id.as_inner();
            let record = application
                .get_record(session)
                .map_err(|_| SessionPromptApplicationError::SessionNotFound(not_found(session)))?;
            sandbox_worktree_config::observe(
                &application,
                session,
                Path::new(record.placement.directory()),
            )
            .await;
            application
                .submit_prompt_run(principal, session_id, request, Some(execution))
                .await
        })
    }

    fn parse_prompt_input(
        &self,
        request: &Value,
    ) -> Result<(String, MessageId, Option<DeliveryMode>, bool), SessionPromptApplicationError>
    {
        let text = request
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                SessionPromptApplicationError::InvalidRequest(invalid("prompt text is absent"))
            })?;
        let text = support::compose_prompt_extensions(request, text)
            .map_err(|error| SessionPromptApplicationError::InvalidRequest(invalid(error)))?;
        let input = request
            .get("id")
            .and_then(Value::as_str)
            .map(|raw| {
                raw.parse::<MessageId>()
                    .map_err(|_| "prompt id is invalid".to_owned())
            })
            .transpose()
            .and_then(|value| match value {
                Some(value) => Ok(value),
                None => format!("msg_{}", self.ids.mint().as_str())
                    .parse()
                    .map_err(|_| "minted prompt id is invalid".to_owned()),
            })
            .map_err(|error| SessionPromptApplicationError::InvalidRequest(invalid(error)))?;
        let delivery = match request.get("delivery").and_then(Value::as_str) {
            Some("steer") => Some(DeliveryMode::Steer),
            Some("queue") => Some(DeliveryMode::Queue),
            None => None,
            Some(_) => {
                return Err(SessionPromptApplicationError::InvalidRequest(invalid(
                    "delivery is invalid",
                )));
            }
        };
        let resume = request
            .get("resume")
            .and_then(Value::as_bool)
            .is_none_or(|value| value);
        Ok((text, input, delivery, resume))
    }

    fn resolve_delivery_and_verify_effects(
        &self,
        session: &SessionId,
        input: &MessageId,
        requested_delivery: Option<DeliveryMode>,
        seat_startup: bool,
    ) -> Result<DeliveryMode, SessionPromptApplicationError> {
        self.ensure_session_writer(session)
            .map_err(|error| SessionPromptApplicationError::Conflict(conflict(error)))?;
        // Busy submissions join the next safe Step. At idle, preserve queued
        // work's admission order; stable retries always keep their stored mode.
        let idle = self.wake_slots.reserve_idle(session);
        let delivery = match requested_delivery {
            Some(delivery) => delivery,
            None => match self.lock_inbox().admitted_delivery(input) {
                Ok(Some(delivery)) => delivery,
                Ok(None) if seat_startup => DeliveryMode::Queue,
                Ok(None) if idle.is_ok() => DeliveryMode::Queue,
                Ok(None) => DeliveryMode::Steer,
                Err(error) => {
                    return Err(SessionPromptApplicationError::Conflict(conflict(error)));
                }
            },
        };
        // A run lost with its process blocks every request. Refuse here, where
        // the user sees the way out, instead of queueing a prompt that fails.
        // A live composition's nested calls also lack terminals until they
        // settle, so only an idle Session's unresolved starts are lost runs.
        if let Ok(idle) = idle {
            let resolved =
                continuation::require_resolved_effects(self.lock_inbox().store(), session);
            drop(idle);
            resolved.map_err(|error| SessionPromptApplicationError::Conflict(conflict(error)))?;
        }
        Ok(delivery)
    }

    #[allow(clippy::too_many_arguments, reason = "prompt admission parameters")]
    fn admit_prompt_entry(
        &self,
        session: &SessionId,
        request: &Value,
        input: &MessageId,
        text: &str,
        delivery: DeliveryMode,
        resume: bool,
        seat_startup: bool,
        child_execution: Option<&ExecutionId>,
    ) -> Result<(u64, Value, bool), SessionPromptApplicationError> {
        let max_steps = grapt_inbox::admission::options::max_steps(request.get("metadata"))
            .map_err(|error| SessionPromptApplicationError::InvalidRequest(invalid(error)))?;
        let images = prompt_images::capture(self, request)
            .map_err(|error| SessionPromptApplicationError::InvalidRequest(invalid(error)))?;
        let claim = if child_execution.is_some() {
            subagent::claim_single_prompt(self, session)
        } else {
            subagent::allow_human_prompt(self, session)
        };
        claim.map_err(|error| SessionPromptApplicationError::Conflict(conflict(error)))?;
        let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
        let origin = if seat_startup {
            grapt_inbox::UserInputOrigin::SeatStartup
        } else {
            grapt_inbox::UserInputOrigin::Human
        };
        let admission = SessionPorts::new(Arc::clone(&self.inbox), &authorities)
            .prompt_with_options(
                session,
                input.clone(),
                text,
                delivery,
                resume,
                grapt_inbox::UserInputOptions {
                    images: &images,
                    origin,
                    max_steps,
                },
            )
            .map_err(|error| {
                SessionPromptApplicationError::Conflict(conflict(error.to_string()))
            })?;
        let (seq, created_ms) = match &admission.outcome {
            AdmitOutcome::Admitted { seq, .. } => (*seq, admission.created_ms),
            AdmitOutcome::Reconciled { .. } => {
                let coordinates = self
                    .lock_inbox()
                    .admission_coordinates(input)
                    .map_err(|error| SessionPromptApplicationError::Conflict(conflict(error)))?;
                (coordinates.seq, coordinates.created_ms)
            }
        };
        if matches!(admission.outcome, AdmitOutcome::Admitted { .. }) {
            input_events::publish(&self.lock_inbox(), &self.hub, session, seq);
            self.agent_activity.send_replace(());
        }
        session_title::automatic(self, session);
        let body = Self::build_prompt_response_body(
            session, input, text, seq, created_ms, delivery, origin,
        );
        Ok((seq, body, should_execute(&admission.outcome)))
    }

    fn prepare_prompt_session(
        &self,
        session: &SessionId,
        request: PromptRequestDto,
    ) -> Result<(Value, String, grapt_types::AgentId), SessionPromptApplicationError> {
        let request = serde_json::to_value(request).map_err(|error| {
            SessionPromptApplicationError::InvalidRequest(invalid(error.to_string()))
        })?;
        let record = self
            .get_record(session)
            .map_err(|_| SessionPromptApplicationError::SessionNotFound(not_found(session)))?;
        if record.launch.is_none() {
            self.launches
                .decide_without_fact(session, self.hub.windows().has_window(session));
        }
        let agent = grapt_types::AgentId::new(record.agent.unwrap_or_else(|| "default".to_owned()))
            .ok_or_else(|| {
                SessionPromptApplicationError::InvalidRequest(invalid(
                    "session agent identity is invalid",
                ))
            })?;
        Ok((request, record.placement.directory().to_owned(), agent))
    }

    fn submit_prompt_run(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: PromptRequestDto,
        child_execution: Option<ExecutionId>,
    ) -> ApplicationFuture<PromptResponseDto, SessionPromptApplicationError> {
        let session = session_id.into_inner();
        let (request, directory, agent) = match self.prepare_prompt_session(&session, request) {
            Ok(prepared) => prepared,
            Err(error) => return ready(Err(error)),
        };
        let seat_startup =
            request.pointer("/metadata/origin").and_then(Value::as_str) == Some("seat-startup");
        let (text, input, requested_delivery, resume) = match self.parse_prompt_input(&request) {
            Ok(parsed) => parsed,
            Err(error) => return ready(Err(error)),
        };
        let delivery = match self.resolve_delivery_and_verify_effects(
            &session,
            &input,
            requested_delivery,
            seat_startup,
        ) {
            Ok(delivery) => delivery,
            Err(error) => return ready(Err(error)),
        };
        let reservation = match self.prompt_mailbox.reserve() {
            Ok(reservation) => reservation,
            Err(error) => {
                return ready(Err(SessionPromptApplicationError::Conflict(conflict(
                    error,
                ))));
            }
        };
        let wake = match self
            .wake_slots
            .reserve_for_workspace(&session, Path::new(&directory))
        {
            Ok(wake) => Some(wake),
            Err(error) => {
                return ready(Err(SessionPromptApplicationError::Conflict(conflict(
                    error,
                ))));
            }
        };
        let (seq, body, should_exec) = match self.admit_prompt_entry(
            &session,
            &request,
            &input,
            &text,
            delivery,
            resume,
            seat_startup,
            child_execution.as_ref(),
        ) {
            Ok(admitted) => admitted,
            Err(error) => return ready(Err(error)),
        };
        let respond = |body: Value| {
            decode(body)
                .map_err(|error| SessionPromptApplicationError::InvalidRequest(invalid(error)))
        };
        if !should_exec {
            return ready(respond(body));
        }
        let accepted = match self.schedule_admitted(
            principal,
            session,
            input,
            seq,
            agent,
            reservation,
            wake,
            child_execution,
            None,
        ) {
            Ok(accepted) => accepted,
            Err(error) => {
                return ready(Err(SessionPromptApplicationError::Conflict(conflict(
                    error,
                ))));
            }
        };
        Box::pin(async move {
            let queued = accepted
                .await
                .map_err(|_| {
                    SessionPromptApplicationError::Conflict(conflict(
                        "execution owner closed before accepting custody",
                    ))
                })?
                .map_err(|error| SessionPromptApplicationError::Conflict(conflict(error)))?;
            let mut body = body;
            if let Some(reason) = queued {
                body["queued"] = json!({ "reason": reason });
            }
            respond(body)
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one admitted execution custody transfer"
    )]
    pub(in crate::dogfood) fn schedule_admitted(
        &self,
        principal: SealedPrincipal,
        session: SessionId,
        input: MessageId,
        seq: u64,
        agent: grapt_types::AgentId,
        reservation: prompt_driver::Reservation,
        wake: Option<super::execution::WakeReservation>,
        child_execution: Option<ExecutionId>,
        native_wake: Option<super::native_mail::custody::NativeWakeFence>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<Option<String>, String>>, String> {
        let (cancel, receiver) = watch::channel(false);
        let stop_waiter = cancel.clone();
        let background = match &child_execution {
            Some(execution) => background_child::BackgroundChild::for_execution(
                self,
                &session,
                seq,
                execution,
                receiver.clone(),
            )?,
            None => None,
        };
        let execution = match child_execution {
            Some(execution) => execution,
            None => self.mint_execution()?,
        };
        let executor = Arc::clone(&self.executor);
        let cancel_executor = Arc::clone(&executor);
        let cancel_session = session.clone();
        let cancel_execution = execution.clone();
        let cancel = Arc::new(move |reason| {
            let _already_settled =
                cancel_executor.shutdown(&cancel_session, &cancel_execution, reason);
            stop_waiter.send_replace(true);
        });
        let (started, accepted) = prompt_driver::Started::channel();
        let hub = Arc::clone(&self.hub);
        let failed_start = started.clone();
        let failed_hub = Arc::clone(&hub);
        let failed_executor = Arc::clone(&executor);
        let failed_session = session.clone();
        let failed_execution = execution.clone();
        let failed = Arc::new(move || {
            failed_start.settle(Err("owned prompt task did not settle normally".into()));
            failed_executor
                .0
                .observe_abandoned(&failed_session, &failed_execution);
            failed_hub.invalidate_session_because(&failed_session, &format!(
                "session {failed_session}: prompt task for execution {failed_execution} did not settle normally"
            ));
        });
        reservation.submit_shutdown(
            execution.clone(),
            Box::pin({
                let application = self.clone();
                let completion = wake.as_ref().map(|wake| wake.completion());
                async move {
                    let _completion = completion;
                    let Some(wake) = application
                        .acquire_wake_custody(&native_wake, wake, &started, receiver.clone())
                        .await
                    else {
                        return;
                    };
                    application
                        .execute_owned_prompt(
                            ExecutionRequest {
                                started,
                                wake,
                                external_notice: native_wake
                                    .as_ref()
                                    .and_then(|fence| fence.external_notice()),
                                session,
                                admission_seq: seq,
                                input,
                                interrupted: receiver,
                                principal,
                                agent,
                                execution,
                            },
                            background,
                        )
                        .await;
                    if let Some(fence) = &native_wake {
                        fence.settle(&application, None).await;
                    }
                }
            }),
            cancel,
            failed,
        );
        Ok(accepted)
    }
    fn build_prompt_response_body(
        session: &SessionId,
        input: &MessageId,
        text: &str,
        seq: u64,
        created_ms: i64,
        delivery: DeliveryMode,
        origin: grapt_inbox::UserInputOrigin,
    ) -> Value {
        let mut body = json!({
            "data": {
                "admittedSeq": seq,
                "id": input.as_str(),
                "sessionID": session.as_str(),
                "timeCreated": created_ms as f64,
                "type": "user",
                "data": {"text": text},
                "delivery": match delivery { DeliveryMode::Steer => "steer", DeliveryMode::Queue => "queue" },
            }
        });
        if origin == grapt_inbox::UserInputOrigin::SeatStartup {
            body["data"]["data"]["metadata"] = json!({"origin":"seat-startup"});
        }
        body
    }

    async fn acquire_wake_custody(
        &self,
        native_wake: &Option<super::native_mail::custody::NativeWakeFence>,
        wake: Option<super::execution::WakeReservation>,
        started: &prompt_driver::Started,
        receiver: watch::Receiver<bool>,
    ) -> Option<Option<super::execution::WakeCustody>> {
        let Some(fence) = native_wake else {
            return Some(wake.map(super::execution::WakeCustody::Reserved));
        };
        let Some(reservation) = wake else {
            started.settle_queued("Native inbox execution custody is unavailable.".into());
            fence.settle(self, None).await;
            return None;
        };
        let acquired = match reservation.enter(started, receiver).await {
            Ok(Some(guard)) => guard,
            Ok(None) => {
                started.settle(Ok(()));
                fence.settle(self, None).await;
                return None;
            }
            Err(reason) => {
                started.settle_queued(reason);
                fence.settle(self, None).await;
                return None;
            }
        };
        if let Err(refusal) = fence.validate_after_wait().await {
            started.settle_queued(refusal.to_string());
            fence.settle(self, Some(refusal)).await;
            return None;
        }
        Some(Some(super::execution::WakeCustody::Acquired(acquired)))
    }
}

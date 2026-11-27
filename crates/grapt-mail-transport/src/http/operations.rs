use super::*;
use crate::ack::SecondaryProjectionState;
use crate::cursor::SubscriptionGeneration;
use crate::port::RemoteSeatMailTransportV1;

impl RemoteSeatMailTransportV1 for HttpMailTransport {
    async fn open(
        &mut self,
        request: OpenRequest,
        limits: ProviderLimits,
    ) -> Result<MailBinding, OperationFailure> {
        if request.requested_protocol().as_str() != self.protocol.as_str() {
            return Err(OpenFailure::ProtocolVersionUnsupported.into());
        }
        let mut args = json!({"request":{"expected_local_project":request.expected_local_project().as_str(),"expected_recipient":request.expected_recipient().as_str(),"requested_protocol":request.requested_protocol().as_str(),"client_instance":request.client_instance().as_str()},"limits":decode::profile_json(limits)});
        if self.protocol == MailTransportVersion::V2 {
            args["request"]["accepted_kinds"] = json!(RemoteSeatMailKind::KNOWN
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>());
        }
        let value = self
            .call(
                "open",
                args,
                limits.max_connect_duration_ticks().get(),
                16384,
                false,
            )
            .await?;
        let binding = value.decode(|value| decode::binding(value, &request, limits))?;
        self.binding = Some(binding.receipt().clone());
        self.subscription = None;
        Ok(binding)
    }
    async fn subscribe_and_snapshot(
        &mut self,
        binding: &MailBinding,
        resume: ResumeRequest,
    ) -> Result<ArmedMailboxSubscription, OperationFailure> {
        self.check(binding)?;
        let limits = binding.receipt().limits();
        let value=self.call("subscribe_and_snapshot",json!({"binding_id":binding.receipt().binding_id().as_str(),"resume":{"from_checkpoint":resume.checkpoint().map(ExclusiveSourceCursor::as_str)}}),limits.provider().max_connect_duration_ticks().get(),16384,false).await?;
        let subscription = value.decode(|value| {
            decode::exact(value, &["subscription_id", "generation", "snapshot"])?;
            let id = decode::text(value, "subscription_id")?;
            if id.len() > limits.provider().max_snapshot_token_bytes().get() {
                return Err(malformed(false));
            }
            Ok(ArmedMailboxSubscription::new(
                MailSubscriptionId::new(id).ok_or_else(|| malformed(false))?,
                SubscriptionGeneration::new(decode::integer(value, "generation")?)
                    .ok_or_else(|| malformed(false))?,
                decode::snapshot(&value["snapshot"], limits)?,
            ))
        })?;
        self.subscription = Some((
            subscription.subscription_id().clone(),
            subscription.generation(),
            limits.provider().max_fetch_duration_ticks().get(),
        ));
        Ok(subscription)
    }
    async fn fetch_snapshot_page(
        &mut self,
        binding: &MailBinding,
        snapshot: &MailSnapshot,
        after: Option<&ExclusiveSourceCursor>,
        page: PageLimits,
    ) -> Result<RemoteMailPage, OperationFailure> {
        self.check(binding)?;
        let limits = binding.receipt().limits();
        if page.max_messages() == 0 || page.max_messages() > limits.messages_per_page() {
            return Err(TransportFailure::PageCountExceeded.into());
        }
        if page.max_aggregate_body_bytes() == 0
            || page.max_aggregate_body_bytes() > limits.aggregate_body_bytes_per_page()
        {
            return Err(TransportFailure::PageBodyBytesExceeded.into());
        }
        let value=self.call("fetch_snapshot_page",json!({"binding_id":binding.receipt().binding_id().as_str(),"snapshot":decode::snapshot_json(snapshot),"after":after.map(ExclusiveSourceCursor::as_str),"page":{"max_messages":page.max_messages(),"max_aggregate_body_bytes":page.max_aggregate_body_bytes()}}),limits.provider().max_fetch_duration_ticks().get(),limits.provider().max_decoded_page_bytes_including_metadata().get().checked_add(128).ok_or_else(|| malformed(false))?,false).await?;
        value.decode(|value| super::page::decode(value, binding, after, page, self.protocol))
    }
    async fn acknowledge_delivery(
        &mut self,
        binding: &MailBinding,
        ack: DeliveryAdmissionAck,
    ) -> Result<DeliveryAckOutcome, OperationFailure> {
        self.check(binding)?;
        if ack.source_cursor().as_str().starts_with("ref1.") {
            return Err(DeliveryAckFailure::MessageNeverEnumerated.into());
        }
        if ack.recipient() != binding.receipt().local_recipient()
            || ack.projects().target() != binding.receipt().projects().target()
            || ack.protocol().as_str() != REMOTE_SEAT_MAIL_TRANSPORT_V1
        {
            return Err(DeliveryAckFailure::AcknowledgementCoordinateMismatch.into());
        }
        let value=self.call("acknowledge_delivery",json!({"binding_id":binding.receipt().binding_id().as_str(),"ack":{
            "ack_id":ack.ack_id().as_str(),"source_binding":ack.source_binding().as_str(),"projects":decode::pair_json(ack.projects()),"source_message":ack.source_message().as_str(),"source_cursor":ack.source_cursor().as_str(),"recipient":ack.recipient().as_str(),"local_message":ack.local_message().as_str(),"local_admission":ack.local_admission().value(),"canonical_message_digest":ack.canonical_message_digest().as_str(),"protocol":ack.protocol().as_str(),
        }}),binding.receipt().limits().provider().max_ack_duration_ticks().get(),16384,true).await?;
        value
            .decode(|value| decode_ack(value, &ack))
            .map_err(|mut e| {
                e.effect = WriteEffect::UnknownRetryExact;
                e
            })
    }
    async fn dismiss(
        &mut self,
        binding: &MailBinding,
        request: BulkDismissRequest,
    ) -> Result<BulkDismissOutcome, OperationFailure> {
        self.check(binding)?;
        let result=self.call("dismiss",json!({"binding_id":binding.receipt().binding_id().as_str(),"request":{"messages":request.messages().iter().map(RemoteMailboxMessageId::as_str).collect::<Vec<_>>(),"reason":request.reason().as_str(),"projects":decode::pair_json(request.projects())}}),binding.receipt().limits().provider().max_ack_duration_ticks().get(),32768,true).await;
        let value = match result {
            Ok(value) => value,
            Err(failure) => {
                if let Some(partial) = &failure.partial_dismiss {
                    let mut ids = std::collections::BTreeSet::new();
                    for id in partial
                        .dismissed
                        .iter()
                        .chain(partial.already_dismissed.iter())
                    {
                        if !request.messages().contains(id) || !ids.insert(id) {
                            return Err(malformed(true));
                        }
                    }
                }
                return Err(failure);
            }
        };
        value
            .decode(|value| {
                decode::exact(value, &["dismissed", "already_dismissed", "projects"])?;
                let projects = decode::pair(&value["projects"])?;
                let dismissed = failure::ids(&value["dismissed"])?;
                let already = failure::ids(&value["already_dismissed"])?;
                let mut all = dismissed.iter().chain(already.iter()).collect::<Vec<_>>();
                all.sort();
                let mut expected = request.messages().iter().collect::<Vec<_>>();
                expected.sort();
                if projects != *request.projects()
                    || all != expected
                    || all.windows(2).any(|v| v[0] == v[1])
                {
                    return Err(malformed(true));
                }
                Ok(BulkDismissOutcome::new(dismissed, already, projects))
            })
            .map_err(|mut e: OperationFailure| {
                e.effect = WriteEffect::UnknownRetryExact;
                e
            })
    }
    fn next_change(
        &mut self,
        subscription: &mut ArmedMailboxSubscription,
    ) -> impl core::future::Future<Output = Result<ChangeSignal, OperationFailure>> + Send + use<>
    {
        let connection = Arc::clone(&self.connection);
        let generation = subscription.generation();
        let coordinates = match (&self.binding, &self.subscription) {
            (Some(binding), Some((id, current_generation, ticks)))
                if id == subscription.subscription_id() && *current_generation == generation =>
            {
                Ok((
                    json!({"binding_id":binding.binding_id().as_str(),"subscription_id":id.as_str(),"generation":generation.value()}),
                    *ticks,
                ))
            }
            _ => Err(OperationFailure::from(SubscriptionClosed::Disconnected)),
        };
        let lease = if coordinates.is_ok()
            && self
                .polling
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            Ok(PollLease(Arc::clone(&self.polling)))
        } else {
            Err(OperationFailure::from(
                TransportFailure::SubscriptionRefused,
            ))
        };
        async move {
            let (args, ticks) = coordinates?;
            let _lease = lease?;
            let value = connection
                .call("next_change", args, ticks, 16384, false)
                .await?;
            value.decode(|value| {
                decode::exact(value, &["generation", "token"])?;
                if decode::integer(value, "generation")? != generation.value()
                    || !value["token"].is_null()
                {
                    return Err(malformed(false));
                }
                Ok(ChangeSignal::new(generation))
            })
        }
    }
    async fn close(
        &mut self,
        binding: MailBinding,
        budget: ShutdownBudget,
    ) -> Result<ShutdownReceipt, OperationFailure> {
        self.check(&binding)?;
        let limits = binding.receipt().limits();
        let ticks = budget
            .max_ticks()
            .min(limits.provider().max_shutdown_duration_ticks().get());
        // Consuming the handle closes the local use path even when the remote
        // join reply is lost. A failure remains a failure, never a join receipt.
        self.binding = None;
        self.subscription = None;
        let value=self.call("close",json!({"binding_id":binding.receipt().binding_id().as_str(),"budget":{"max_ticks":ticks,"max_joined_tasks":budget.max_joined_tasks()}}),ticks,16384,false).await?;
        let (joined, used) = value.decode(|value| {
            decode::exact(value, &["joined_tasks", "ticks_used"])?;
            let joined = value["joined_tasks"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| malformed(false))?;
            let used = value["ticks_used"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| malformed(false))?;
            if joined > budget.max_joined_tasks() || used > ticks {
                return Err(malformed(false));
            }
            Ok((joined, used))
        })?;
        self.binding = None;
        self.subscription = None;
        Ok(ShutdownReceipt::new(joined, used))
    }
}
pub(super) fn decode_ack(
    value: &Value,
    ack: &DeliveryAdmissionAck,
) -> Result<DeliveryAckOutcome, OperationFailure> {
    decode::exact(
        value,
        &["tag", "receipt", "primary_delivery", "floor_projection"],
    )?;
    decode::exact(&value["primary_delivery"], &["witnessed"])?;
    if value["primary_delivery"]["witnessed"] != Value::Bool(true) {
        return Err(malformed(true));
    } // NMAIL:primary-proof
    let receipt = &value["receipt"];
    decode::exact(receipt, &["ack_id", "source_message", "projects"])?;
    if decode::text(receipt, "ack_id")? != ack.ack_id().as_str()
        || decode::text(receipt, "source_message")? != ack.source_message().as_str()
        || decode::pair(&receipt["projects"])? != *ack.projects()
    {
        return Err(malformed(true));
    }
    let p = &value["floor_projection"];
    decode::exact(
        p,
        &[
            "state",
            "outcome",
            "disposition",
            "reason",
            "outcome_record",
            "retry_required",
            "retry",
        ],
    )?;
    let state = match decode::text(p, "state")? {
        "posted" => SecondaryProjectionState::Posted,
        "already-present" => SecondaryProjectionState::AlreadyPresent,
        "unarmed" => SecondaryProjectionState::Unarmed,
        "refused" => SecondaryProjectionState::Refused,
        "unreadable" => SecondaryProjectionState::Unreadable,
        _ => return Err(malformed(true)),
    };
    let written = matches!(
        state,
        SecondaryProjectionState::Posted | SecondaryProjectionState::AlreadyPresent
    );
    let expected_outcome = if written {
        "written"
    } else if state == SecondaryProjectionState::Unreadable {
        "unreadable"
    } else {
        "refused"
    };
    let expected_disposition = if written {
        p["state"].clone()
    } else {
        Value::Null
    };
    let expected_reason = if written {
        Value::Null
    } else {
        json!(format!("secondary-{}", decode::text(p, "state")?))
    };
    decode::exact(&p["outcome_record"], &["state"])?;
    if decode::text(p, "outcome")? != expected_outcome
        || p["disposition"] != expected_disposition
        || p["reason"] != expected_reason
        || !matches!(
            decode::text(&p["outcome_record"], "state")?,
            "recorded" | "unrecorded" | "unreadable"
        )
    {
        return Err(malformed(true));
    }
    let repair = matches!(
        state,
        SecondaryProjectionState::Refused | SecondaryProjectionState::Unreadable
    );
    if p["retry_required"] != Value::Bool(repair) {
        return Err(malformed(true));
    }
    if repair {
        decode::exact(&p["retry"], &["operation", "exact_coordinates_required"])?;
        if p["retry"]["operation"] != json!("acknowledge_delivery")
            || p["retry"]["exact_coordinates_required"] != json!(true)
        {
            return Err(malformed(true));
        }
    } else if !p["retry"].is_null() {
        return Err(malformed(true));
    }
    let receipt = DeliveryAckReceipt::new(
        ack.ack_id().clone(),
        ack.source_message().clone(),
        ack.projects().clone(),
    )
    .with_projection(state);
    match decode::text(value, "tag")? {
        "Recorded" => Ok(DeliveryAckOutcome::Recorded(receipt)),
        "ExactRetry" => Ok(DeliveryAckOutcome::ExactRetry(receipt)),
        _ => Err(malformed(true)),
    }
}

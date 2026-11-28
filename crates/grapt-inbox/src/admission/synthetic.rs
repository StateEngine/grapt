use super::existing::ExistingAdmission;
use super::*;

#[derive(Debug)]
pub struct SyntheticAdmitRequest<'a> {
    pub event_id: &'a EventId,
    pub session: &'a SessionId,
    pub input: &'a MessageId,
    pub text: &'a str,
    pub description: Option<&'a str>,
    pub metadata: Option<&'a Value>,
    pub delivery: DeliveryMode,
    pub resume: bool,
    pub created_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotedInputKind {
    User,
    Synthetic,
}

impl PromotedInputKind {
    pub(super) fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "user" => Some(Self::User),
            "synthetic" => Some(Self::Synthetic),
            _ => None,
        }
    }
}

impl Inbox {
    /// Admit a newly authorized synthetic input. The explicit marker is what
    /// separates this write path from imported oracle-era synthetic rows,
    /// which remain audit-only during migration and replay.
    pub fn admit_synthetic(
        &mut self,
        request: SyntheticAdmitRequest<'_>,
    ) -> Result<AdmitOutcome, InboxError> {
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        let outcome = Self::admit_synthetic_within(&tx, &held, &request)?;
        tx.commit()?.discard();
        Ok(outcome)
    }

    /// Admit native work inside its owner's existing transaction, so an
    /// authorized followup can bind its operation and input atomically.
    pub fn admit_synthetic_within(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        request: &SyntheticAdmitRequest<'_>,
    ) -> Result<AdmitOutcome, InboxError> {
        Self::admit_synthetic_owned_within(tx, held, request, false)
    }

    pub(super) fn admit_synthetic_owned_within(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        request: &SyntheticAdmitRequest<'_>,
        native_mail_wake: bool,
    ) -> Result<AdmitOutcome, InboxError> {
        const MAX_TEXT_BYTES: usize = 256 * 1_024;
        const MAX_DESCRIPTION_BYTES: usize = 4 * 1_024;
        const MAX_METADATA_BYTES: usize = 64 * 1_024;
        if request.text.is_empty() || request.text.len() > MAX_TEXT_BYTES {
            return Err(InboxError::PendingBound("synthetic text bytes"));
        }
        if request
            .description
            .is_some_and(|value| value.len() > MAX_DESCRIPTION_BYTES)
        {
            return Err(InboxError::PendingBound("synthetic description bytes"));
        }
        let metadata_bytes = request
            .metadata
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| InboxError::PendingBound("synthetic metadata encoding"))?;
        if metadata_bytes
            .as_ref()
            .is_some_and(|value| value.len() > MAX_METADATA_BYTES)
        {
            return Err(InboxError::PendingBound("synthetic metadata bytes"));
        }
        let wake = if request.resume {
            WakeDisposition::WakeAdvised
        } else {
            WakeDisposition::AdmitOnly
        };
        let aggregate = AggregateId::Session(request.session.clone());
        if let Some(existing) = Self::find_existing(tx, request.input)? {
            return Self::reconcile_synthetic(request, existing, wake);
        }
        let mut data = json!({
            "sessionID": request.session.as_str(),
            "inputID": request.input.as_str(),
            "input": {
                "type": "synthetic",
                "rustAuthorized": true,
                "data": {
                    "text": request.text,
                },
                "delivery": delivery_str(request.delivery),
            },
        });
        if let Some(description) = request.description {
            data["input"]["data"]["description"] = json!(description);
        }
        if let Some(metadata) = request.metadata {
            data["input"]["data"]["metadata"] = metadata.clone();
        }
        if native_mail_wake {
            // This marker is outside caller-controlled metadata. Only the
            // native-mail owner paths can authorize automatic recovery.
            data["input"]["nativeMailWake"] = json!(true);
            data["input"]["nativeMailTarget"] = json!(super::native_mail_state::admission_target(
                tx,
                request.session,
                request.metadata
            )?
            .as_ref()
            .map(EventId::as_str));
        }
        let version = EventVersion::new(1).ok_or(InboxError::Version)?;
        let appended = EventStore::append_within(
            tx,
            AppendInput {
                id: request.event_id,
                aggregate,
                writer: held.token(&AggregateId::Session(request.session.clone()))?,
                kind_name: "session.input.admitted",
                version,
                created_ms: request.created_ms,
                data: &data,
            },
            &mut [&mut InboxProjector],
        )?;
        let seq = match appended {
            Appended::Committed { seq } | Appended::Idempotent { seq } => seq,
        };
        Ok(AdmitOutcome::Admitted { seq, wake })
    }

    fn reconcile_synthetic(
        request: &SyntheticAdmitRequest<'_>,
        existing: ExistingAdmission,
        wake: WakeDisposition,
    ) -> Result<AdmitOutcome, InboxError> {
        if existing.session != request.session.as_str() {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "session",
            });
        }
        if existing.delivery != delivery_str(request.delivery) {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "delivery",
            });
        }
        if existing.input_type != "synthetic" {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "type",
            });
        }
        if existing.text != request.text {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "text",
            });
        }
        if existing.description.as_deref() != request.description {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "description",
            });
        }
        if existing.metadata.as_ref() != request.metadata {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "metadata",
            });
        }
        let wake = if existing.retired {
            WakeDisposition::Settled
        } else {
            wake
        };
        Ok(AdmitOutcome::Reconciled { wake })
    }
}
